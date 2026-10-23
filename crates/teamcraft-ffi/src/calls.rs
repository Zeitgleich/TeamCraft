//! Calls for the mobile apps: the engine thread prepares a join (token, device signer, verified
//! devices; e2ee.md §8) and the media runs in `teamcraft-media`'s native driver on a call thread of
//! its own. The app integrates the call with the platform (Android Telecom) and drives it through
//! [`CallSession`]; [`CallListener`] tells it about rings and the call's state.
//!
//! Network changes: the app reports them with [`CallSession::network_changed`] (Android
//! `ConnectivityManager` callbacks, iOS `NWPathMonitor`). The connection migrates to the new
//! network, or the call rejoins with a fresh token from the engine thread; meanwhile the state is
//! [`CallPhase::Reconnecting`].

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};

use teamcraft_core::client::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_crypto::identity::VerifiedDevice;
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioSink, AudioSource};
use teamcraft_media::audio_processing::AudioProcessingConfig;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_cues::StatusCues;
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{
  JoinTicketSource, NativeCall, NativeCallConfig, NativeCallError, TicketFuture,
};
use teamcraft_media::call_resilience::{JoinTicket, JoinTicketError};
use teamcraft_media::datagram::SimulcastLayer;
use teamcraft_media::openh264_library;
use teamcraft_media::rtp::RtpTimestamp;
use teamcraft_media::video::{EncodedVideoFrame, VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::CaptureFormat;
use teamcraft_media::video_pipeline::{VideoSendConfig, preferred_encoders};
use teamcraft_proto::{
  AudioCodec, CallJoinTokenResponse, ConversationId, DeviceId, GatewayEvent, MediaCapabilities,
  MediaStreamId, UserId, VideoCodec, VideoLayerInfo, VideoTrack, VideoTrackKind,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::engine::Job;
use crate::error::TeamCraftError;
use crate::video::{AppCamera, CameraFrame, CameraSlot, VideoFrameRgba, i420_to_rgba};

/// Worker threads of a call's runtime: network and engine; audio has its own thread.
const CALL_WORKER_THREADS: usize = 2;
/// Simulcast layers a phone sends: its uplink and encoder are small.
const CAMERA_LAYERS: u8 = 2;
const CAMERA_BITRATE_BPS: u32 = 800_000;
/// The highest layer a phone receives of other devices' tracks (0 is the smallest).
const RECEIVE_MAX_LAYER: u8 = 1;

/// Told about rings and about the call this device is in. Called from the engine and call
/// threads; implementations hand over to their UI thread.
#[uniffi::export(with_foreign)]
pub trait CallListener: Send + Sync {
  fn on_call_state(&self, state: CallStateInfo);
  /// Someone calls this user in a conversation (ring it, e.g. as an incoming Telecom call).
  fn on_incoming_call(&self, call: IncomingCallInfo);
  /// The conversation's call stopped ringing (answered elsewhere, declined or ended).
  fn on_ring_ended(&self, conversation_id: String);
}

/// Told about other devices' video in the call and their decoded frames (on decoder threads).
#[uniffi::export(with_foreign)]
pub trait VideoListener: Send + Sync {
  /// The other devices' video tracks now in the call; each is received and decoded.
  fn on_video_tracks(&self, tracks: Vec<VideoTrackInfo>);
  fn on_video_frame(&self, frame: VideoFrameRgba);
}

/// Video for apps that do the codec work themselves (iOS: VideoToolbox; OpenH264 can't be
/// downloaded into an iOS app). The engine encrypts, packetizes and forwards H.264 access units in
/// Annex B form and hands received ones over undecoded. Used instead of [`VideoListener`] and
/// `start_camera`: while a listener is set, the core neither decodes nor encodes video itself.
/// Called from the call thread; implementations hand over to their own queues.
#[uniffi::export(with_foreign)]
pub trait EncodedVideoListener: Send + Sync {
  /// Another device published a camera or screen track; call `watch_encoded_video` to receive it.
  fn on_remote_video_track(&self, track: RemoteVideoTrack);
  /// A track was unpublished or its device left.
  fn on_remote_video_ended(&self, stream_id: String);
  /// One encoded frame of a watched track.
  fn on_remote_video_frame(&self, frame: RemoteVideoFrame);
  /// Encode a keyframe (with SPS and PPS) of the camera soon: a receiver joined or lost a frame.
  fn on_keyframe_requested(&self);
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteVideoTrack {
  pub stream_id: String,
  /// The publishing device.
  pub device_id: String,
  pub is_screen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RemoteVideoFrame {
  pub stream_id: String,
  pub is_keyframe: bool,
  /// 90 kHz RTP clock.
  pub timestamp: u32,
  /// An H.264 access unit in Annex B form; keyframes carry SPS and PPS.
  pub data: Vec<u8>,
}

/// The app's handle on the call this device is in.
#[derive(uniffi::Object)]
pub struct CallSession {
  controls: mpsc::UnboundedSender<Control>,
  camera: CameraSlot,
}

/// Another device's video track.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct VideoTrackInfo {
  pub stream_id: u32,
  pub device_id: String,
  pub is_screen: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CallStateInfo {
  pub conversation_id: String,
  pub phase: CallPhase,
  pub is_muted: bool,
  /// Nothing of the call is played.
  pub is_deafened: bool,
  /// This device sends its camera.
  pub is_camera_on: bool,
  /// End-to-end encrypted with every participant's device verified.
  pub is_secured: bool,
  /// Other devices in the call.
  pub participant_count: u32,
  /// Of those, the ones whose connection was lost and who are rejoining.
  pub reconnecting_count: u32,
  /// The code participants compare to rule out a man in the middle (once secured).
  pub maybe_privacy_code: Option<String>,
  /// Why the call ended (with [`CallPhase::Ended`]).
  pub maybe_end_reason: Option<CallEndReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallPhase {
  Connecting,
  Connected,
  /// The connection was lost; the call is rejoining (media paused). Show "reconnecting", not
  /// "ended": the others keep this device's place for a while.
  Reconnecting,
  Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallEndReason {
  Left,
  RemovedByHost,
  EndedByHost,
  /// The connection to the call broke (and rejoining failed within the grace period).
  Failed,
}

/// Which call status sounds play: a soft repeating tone while this device reconnects, short
/// tones when it is back or the call is lost, and a quieter one when someone else drops. They are
/// mixed into the call's own playback, so they follow its route (earpiece, speaker, Bluetooth).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallStatusSounds {
  All,
  /// Only the sounds about this device's own connection.
  OwnOnly,
  Off,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct IncomingCallInfo {
  pub conversation_id: String,
  pub caller_user_id: String,
}

/// Everything joining a conversation's call needs from the core (e2ee.md §8).
pub(crate) struct CallPreparation {
  pub(crate) conversation: ConversationId,
  pub(crate) join: CallJoinTokenResponse,
  pub(crate) user_id: UserId,
  pub(crate) device_id: DeviceId,
  /// Signs this device's per-call DAVE key; the device key itself stays in the core.
  pub(crate) signer: CallKeySigner,
  /// The conversation's devices (this one included) whose certificates verified.
  pub(crate) devices: Vec<VerifiedDevice>,
}

/// Where call events meet the app's listener: the engine feeds it gateway events, call threads
/// feed it the call's state.
#[derive(Clone, Default)]
pub(crate) struct CallHub {
  inner: Arc<Mutex<HubState>>,
}

#[derive(Default)]
struct HubState {
  maybe_listener: Option<Arc<dyn CallListener>>,
  maybe_active: Option<ConversationId>,
}

enum Control {
  SetMuted(bool),
  SetDeafened(bool),
  StartCamera {
    format: CaptureFormat,
    done: oneshot::Sender<Result<(), TeamCraftError>>,
  },
  StopCamera,
  SetVideoListener(Arc<dyn VideoListener>),
  /// Hold: nothing is sent and nothing is played until released.
  SetHeld(bool),
  SetEncodedVideoListener(Arc<dyn EncodedVideoListener>),
  PublishEncodedCamera(VideoLayerInfo),
  SendEncodedCameraFrame(EncodedVideoFrame),
  StopEncodedCamera,
  WatchEncodedVideo(MediaStreamId),
  UnwatchEncodedVideo(MediaStreamId),
  NetworkChanged,
  SetStatusSounds(CallStatusSounds),
  Leave,
}

/// Other devices' tracks being watched, and who hears about them.
#[derive(Default)]
struct RemoteVideo {
  tracks: BTreeMap<MediaStreamId, VideoTrackInfo>,
  listener: SharedVideoListener,
}

/// The decoder threads' sinks read the listener when a frame arrives, so it may come later.
type SharedVideoListener = Arc<Mutex<Option<Arc<dyn VideoListener>>>>;

/// Lets the media layer sign with the core's signer.
struct CoreSigner(CallKeySigner);

/// Fresh join tokens from the engine thread, for rejoining after a lost connection.
struct EngineJoinTickets {
  jobs: mpsc::UnboundedSender<Job>,
  conversation: ConversationId,
}

#[uniffi::export]
impl CallSession {
  pub fn set_muted(&self, is_muted: bool) {
    self.send(Control::SetMuted(is_muted));
  }

  /// Stops playing the call (and mutes, as on desktop).
  pub fn set_deafened(&self, is_deafened: bool) {
    self.send(Control::SetDeafened(is_deafened));
  }

  /// Other devices' video and frames go to `listener` from now on.
  pub fn set_video_listener(&self, listener: Arc<dyn VideoListener>) {
    self.send(Control::SetVideoListener(listener));
  }

  /// Publishes the camera: the app pushes frames of this size and rate with
  /// [`CallSession::push_camera_frame`] (upright after the frame's rotation). Encoded in hardware
  /// (MediaCodec) where the phone can.
  pub async fn start_camera(&self, width: u16, height: u16, framerate: u8) -> Result<(), TeamCraftError> {
    let (done, done_rx) = oneshot::channel();
    let format = CaptureFormat {
      resolution: upright(width, height),
      framerate,
    };
    self.send(Control::StartCamera { format, done });
    done_rx.await.map_err(|_| TeamCraftError::EngineStopped)?
  }

  /// One camera image; `false` while the camera isn't published (or the image is malformed).
  /// Called from the camera's thread; doesn't wait for the call.
  pub fn push_camera_frame(&self, frame: CameraFrame) -> bool {
    self.camera.push(&frame)
  }

  pub fn stop_camera(&self) {
    self.camera.clear();
    self.send(Control::StopCamera);
  }

  /// Puts the call on hold (or takes it off): the microphone is silenced and the others aren't
  /// played. The call stays joined, so resuming is instant.
  pub fn set_held(&self, is_held: bool) {
    self.send(Control::SetHeld(is_held));
  }

  /// The phone's network changed (another network became the default, or the current one was
  /// lost or changed its addresses). The call checks that its connection followed and rejoins
  /// otherwise; while rejoining, it retries at once.
  pub fn network_changed(&self) {
    self.send(Control::NetworkChanged);
  }

  /// Which call status sounds play (the app's setting); call it after joining.
  pub fn set_status_sounds(&self, sounds: CallStatusSounds) {
    self.send(Control::SetStatusSounds(sounds));
  }

  /// Leaves the call; the listener gets the ended state.
  pub fn leave(&self) {
    self.send(Control::Leave);
  }

  /// Remote video and keyframe requests go to `listener` from now on, as encoded frames (see
  /// [`EncodedVideoListener`]); the core stops decoding remote video itself.
  pub fn set_encoded_video_listener(&self, listener: Arc<dyn EncodedVideoListener>) {
    self.send(Control::SetEncodedVideoListener(listener));
  }

  /// Publishes the camera as one H.264 layer; frames follow with `send_encoded_camera_frame`.
  pub fn publish_encoded_camera(&self, width: u16, height: u16, max_framerate: u8, max_bitrate_bps: u32) {
    self.send(Control::PublishEncodedCamera(VideoLayerInfo {
      layer: 0,
      width,
      height,
      max_framerate,
      max_bitrate_bps,
    }));
  }

  /// One encoded camera frame (H.264, Annex B, SPS and PPS before every keyframe). Dropped until
  /// the call is secured and the SFU confirmed the track.
  pub fn send_encoded_camera_frame(&self, data: Vec<u8>, timestamp: u32, is_keyframe: bool) {
    self.send(Control::SendEncodedCameraFrame(EncodedVideoFrame {
      codec: VideoCodec::H264,
      layer: SimulcastLayer::BASE,
      timestamp: RtpTimestamp(timestamp),
      is_keyframe,
      data,
    }));
  }

  pub fn stop_encoded_camera(&self) {
    self.send(Control::StopEncodedCamera);
  }

  /// Receives the track `stream_id` (from `on_remote_video_track`).
  pub fn watch_encoded_video(&self, stream_id: String) {
    if let Some(stream) = parse_stream(&stream_id) {
      self.send(Control::WatchEncodedVideo(stream));
    }
  }

  pub fn unwatch_encoded_video(&self, stream_id: String) {
    if let Some(stream) = parse_stream(&stream_id) {
      self.send(Control::UnwatchEncodedVideo(stream));
    }
  }
}

impl CallSession {
  fn send(&self, control: Control) {
    // The call already ended if its thread is gone; the listener was told then.
    let _ = self.controls.send(control);
  }
}

impl CallHub {
  pub(crate) fn set_listener(&self, listener: Arc<dyn CallListener>) {
    lock(&self.inner).maybe_listener = Some(listener);
  }

  /// Rings from the gateway, for the core's call event sink.
  pub(crate) fn on_gateway_event(&self, event: &GatewayEvent) {
    let (maybe_listener, maybe_active) = {
      let state = lock(&self.inner);
      (state.maybe_listener.clone(), state.maybe_active)
    };
    let Some(listener) = maybe_listener else {
      return;
    };
    match event {
      GatewayEvent::CallRing {
        conversation_id,
        from_user_id,
        ..
      } if maybe_active != Some(*conversation_id) => listener.on_incoming_call(IncomingCallInfo {
        conversation_id: conversation_id.to_string(),
        caller_user_id: from_user_id.to_string(),
      }),
      GatewayEvent::CallUpdate {
        conversation_id,
        is_ringing: false,
        ..
      }
      | GatewayEvent::CallEnd { conversation_id, .. } => listener.on_ring_ended(conversation_id.to_string()),
      _ => {},
    }
  }

  fn report(&self, state: CallStateInfo) {
    let maybe_listener = {
      let mut hub = lock(&self.inner);
      if state.phase == CallPhase::Ended {
        hub.maybe_active = None;
      }
      hub.maybe_listener.clone()
    };
    if let Some(listener) = maybe_listener {
      listener.on_call_state(state);
    }
  }
}

/// Joins the prepared call on a new call thread; returns once connected.
pub(crate) async fn join(
  preparation: CallPreparation,
  hub: CallHub,
  jobs: mpsc::UnboundedSender<Job>,
) -> Result<Arc<CallSession>, TeamCraftError> {
  let conversation = preparation.conversation;
  let own_device = preparation.device_id;
  let mut config = native_config(preparation)?;
  config.maybe_ticket_source = Some(Arc::new(EngineJoinTickets { jobs, conversation }));
  let camera = CameraSlot::default();
  let thread_camera = camera.clone();
  let (controls, control_rx) = mpsc::unbounded_channel();
  let (joined, joined_rx) = oneshot::channel();
  lock(&hub.inner).maybe_active = Some(conversation);
  let mut state = CallStateInfo {
    conversation_id: conversation.to_string(),
    phase: CallPhase::Connecting,
    is_muted: false,
    is_deafened: false,
    is_camera_on: false,
    is_secured: false,
    participant_count: 0,
    reconnecting_count: 0,
    maybe_privacy_code: None,
    maybe_end_reason: None,
  };
  hub.report(state.clone());
  let thread_hub = hub.clone();
  let spawned = std::thread::Builder::new()
    .name("teamcraft-call".into())
    .spawn(move || {
      let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(CALL_WORKER_THREADS)
        .enable_all()
        .build()
      {
        Ok(runtime) => runtime,
        Err(error) => {
          // The joining caller waits for this; if it has gone, nobody needs the answer.
          let _ = joined.send(Err(TeamCraftError::Call(error.to_string())));
          return;
        },
      };
      runtime.block_on(async move {
        let call = match NativeCall::join(config).await {
          Ok(call) => call,
          Err(error) => {
            let _ = joined.send(Err(TeamCraftError::Call(error.to_string())));
            return;
          },
        };
        let _ = joined.send(Ok(()));
        let context = CallContext {
          own_device,
          camera: thread_camera,
          hub: thread_hub,
        };
        run_call(call, control_rx, &context, &mut state).await;
      });
    });
  if let Err(error) = spawned {
    lock(&hub.inner).maybe_active = None;
    return Err(TeamCraftError::Call(error.to_string()));
  }
  let result = joined_rx.await.unwrap_or(Err(TeamCraftError::EngineStopped));
  if let Err(error) = &result {
    warn!(%error, "joining the call failed");
    hub.report(ended(conversation, CallEndReason::Failed));
  }
  result.map(|()| Arc::new(CallSession { controls, camera }))
}

/// What the call thread needs besides the call.
struct CallContext {
  own_device: DeviceId,
  camera: CameraSlot,
  hub: CallHub,
}

/// Applies the app's controls and reports the engine's events until the call ends.
async fn run_call(
  mut call: NativeCall,
  mut controls: mpsc::UnboundedReceiver<Control>,
  context: &CallContext,
  state: &mut CallStateInfo,
) {
  let mut reason = CallEndReason::Left;
  let mut reconnecting = BTreeSet::new();
  let mut video = RemoteVideo::default();
  let mut encoded = EncodedVideoBridge::default();
  let mut audio = AudioControls::default();
  loop {
    tokio::select! {
      maybe_control = controls.recv() => match maybe_control {
        // Leave asked for, or the app dropped the session.
        Some(Control::Leave) | None => break,
        Some(Control::SetMuted(is_muted)) => {
          audio.is_muted = is_muted;
          call.set_muted(audio.is_silent());
        },
        Some(Control::NetworkChanged) => call.network_changed(),
        Some(Control::SetStatusSounds(sounds)) => call.set_status_cues(status_cues(sounds)),
        Some(Control::SetHeld(is_held)) => {
          audio.is_held = is_held;
          call.set_muted(audio.is_silent());
          call.set_deafened(is_held);
          state.is_deafened = is_held;
          context.hub.report(state.clone());
        },
        Some(control) => {
          apply_encoded_control(&call, &mut video, &mut encoded, state, &control);
          apply_control(&call, control, context, state, &video).await;
          context.hub.report(state.clone());
        },
      },
      maybe_event = call.next_event() => match maybe_event {
        Some(event) => {
          encoded.report(context.own_device, &event);
          if !encoded.is_active() {
            watch_video(&call, &event, context.own_device, &mut video);
          }
          if let Some(end) = apply_event(state, &mut reconnecting, event) {
            reason = end;
            break;
          }
          context.hub.report(state.clone());
        },
        None => {
          reason = CallEndReason::Failed;
          break;
        },
      },
    }
  }
  context.camera.clear();
  match call.leave().await {
    Ok(_) => {},
    Err(NativeCallError::ConnectionLost) => {
      warn!("the connection to the call was lost");
      reason = CallEndReason::Failed;
    },
    Err(error) => info!(%error, "the call ended with an error"),
  }
  state.phase = CallPhase::Ended;
  state.maybe_end_reason = Some(reason);
  context.hub.report(state.clone());
}

async fn apply_control(
  call: &NativeCall,
  control: Control,
  context: &CallContext,
  state: &mut CallStateInfo,
  video: &RemoteVideo,
) {
  match control {
    Control::SetMuted(is_muted) => call.set_muted(is_muted),
    Control::SetDeafened(is_deafened) => {
      call.set_deafened(is_deafened);
      state.is_deafened = is_deafened;
    },
    Control::StartCamera { format, done } => {
      let result = start_camera(call, format, context.camera.clone()).await;
      state.is_camera_on = result.is_ok();
      // The app may have stopped waiting; the state report tells it anyway.
      let _ = done.send(result);
    },
    Control::StopCamera => {
      call.stop_video(VideoTrackKind::Camera);
      state.is_camera_on = false;
    },
    Control::SetVideoListener(listener) => {
      listener.on_video_tracks(video.tracks.values().cloned().collect());
      *lock_listener(&video.listener) = Some(listener);
    },
    // Handled by the loop and `apply_encoded_control`.
    Control::SetHeld(_)
    | Control::SetEncodedVideoListener(_)
    | Control::PublishEncodedCamera(_)
    | Control::SendEncodedCameraFrame(_)
    | Control::StopEncodedCamera
    | Control::WatchEncodedVideo(_)
    | Control::UnwatchEncodedVideo(_)
    | Control::NetworkChanged
    | Control::SetStatusSounds(_)
    | Control::Leave => {},
  }
}

async fn start_camera(
  call: &NativeCall,
  format: CaptureFormat,
  camera: CameraSlot,
) -> Result<(), TeamCraftError> {
  let config = VideoSendConfig {
    kind: VideoTrackKind::Camera,
    capture: Box::new(AppCamera::new(format, camera)),
    layers: simulcast_layers(
      format.resolution,
      format.framerate,
      CAMERA_BITRATE_BPS,
      CAMERA_LAYERS,
    ),
    encoders: preferred_encoders(),
    maybe_filter: None,
  };
  call
    .start_video(config)
    .await
    .map_err(|error| TeamCraftError::Call(error.to_string()))
}

/// Receives other devices' tracks as they come and go, and tells the app.
fn watch_video(call: &NativeCall, event: &CallEngineEvent, own_device: DeviceId, video: &mut RemoteVideo) {
  match event {
    CallEngineEvent::VideoPublished(track) if track.device_id != own_device => {
      let stream_id = track.stream_id;
      let listener = video.listener.clone();
      call.watch_video(
        stream_id,
        Some(RECEIVE_MAX_LAYER),
        Box::new(move |frame| {
          let maybe_listener = lock_listener(&listener).clone();
          if let Some(listener) = maybe_listener {
            let resolution = frame.resolution();
            listener.on_video_frame(VideoFrameRgba {
              stream_id: stream_id.0,
              width: u32::from(resolution.width),
              height: u32::from(resolution.height),
              rgba: i420_to_rgba(&frame),
            });
          }
        }),
      );
      video.tracks.insert(stream_id, track_info(track));
    },
    CallEngineEvent::VideoUnpublished(stream_id) => {
      if video.tracks.remove(stream_id).is_none() {
        return;
      }
      call.unwatch_video(*stream_id);
    },
    _ => return,
  }
  let maybe_listener = lock_listener(&video.listener).clone();
  if let Some(listener) = maybe_listener {
    listener.on_video_tracks(video.tracks.values().cloned().collect());
  }
}

/// Mute by the app and hold, which silence the microphone together.
#[derive(Default)]
struct AudioControls {
  is_muted: bool,
  is_held: bool,
}

impl AudioControls {
  fn is_silent(&self) -> bool {
    self.is_muted || self.is_held
  }
}

/// What the call thread keeps for an app that encodes and decodes video itself.
#[derive(Default)]
struct EncodedVideoBridge {
  maybe_listener: Option<Arc<dyn EncodedVideoListener>>,
  /// The stream id of this device's camera once the SFU announced it.
  maybe_camera: Option<MediaStreamId>,
  /// The camera was asked for; the track announcement may still be on its way.
  is_camera_requested: bool,
  /// Tracks of other devices that are published now; a listener that registers late (the app sets
  /// it after the join returned) learns about them when it registers.
  remote_tracks: Vec<RemoteVideoTrack>,
}

impl EncodedVideoBridge {
  fn is_active(&self) -> bool {
    self.maybe_listener.is_some()
  }

  fn set_listener(&mut self, listener: Arc<dyn EncodedVideoListener>) {
    for track in &self.remote_tracks {
      listener.on_remote_video_track(track.clone());
    }
    self.maybe_listener = Some(listener);
  }

  /// Tells the app's listener what the engine reported, and notes the camera's stream.
  fn report(&mut self, own_device: DeviceId, event: &CallEngineEvent) {
    match event {
      CallEngineEvent::VideoPublished(track) if track.device_id == own_device => {
        if track.kind == VideoTrackKind::Camera && self.is_camera_requested {
          self.maybe_camera = Some(track.stream_id);
        }
      },
      CallEngineEvent::VideoPublished(track) => {
        let remote = RemoteVideoTrack {
          stream_id: track.stream_id.0.to_string(),
          device_id: track.device_id.to_string(),
          is_screen: track.kind == VideoTrackKind::Screen,
        };
        self.remote_tracks.push(remote.clone());
        if let Some(listener) = &self.maybe_listener {
          listener.on_remote_video_track(remote);
        }
      },
      CallEngineEvent::VideoUnpublished(stream_id) => {
        if self.maybe_camera == Some(*stream_id) {
          self.maybe_camera = None;
        } else {
          let id = stream_id.0.to_string();
          self.remote_tracks.retain(|track| track.stream_id != id);
          if let Some(listener) = &self.maybe_listener {
            listener.on_remote_video_ended(id);
          }
        }
      },
      CallEngineEvent::VideoFrame(frame) => {
        if let Some(listener) = &self.maybe_listener {
          listener.on_remote_video_frame(RemoteVideoFrame {
            stream_id: frame.stream_id.0.to_string(),
            is_keyframe: frame.is_keyframe,
            timestamp: frame.timestamp.0,
            data: frame.data.clone(),
          });
        }
      },
      CallEngineEvent::KeyframeRequested { stream_id, .. } if self.maybe_camera == Some(*stream_id) => {
        if let Some(listener) = &self.maybe_listener {
          listener.on_keyframe_requested();
        }
      },
      _ => {},
    }
  }
}

/// Camera and watch requests of an app that does the codec work itself.
fn apply_encoded_control(
  call: &NativeCall,
  video: &mut RemoteVideo,
  encoded: &mut EncodedVideoBridge,
  state: &mut CallStateInfo,
  control: &Control,
) {
  match control {
    Control::SetEncodedVideoListener(listener) => {
      // Tracks the core already decodes itself are handed over to the app.
      for stream_id in std::mem::take(&mut video.tracks).into_keys() {
        call.unwatch_video(stream_id);
      }
      encoded.set_listener(listener.clone());
    },
    Control::PublishEncodedCamera(layer) => {
      if !encoded.is_camera_requested {
        encoded.is_camera_requested = true;
        state.is_camera_on = true;
        call.publish_video(VideoTrackKind::Camera, VideoCodec::H264, vec![*layer]);
      }
    },
    Control::SendEncodedCameraFrame(frame) => {
      if let Some(stream) = encoded.maybe_camera {
        call.send_video(stream, frame.clone());
      }
    },
    Control::StopEncodedCamera => {
      encoded.is_camera_requested = false;
      state.is_camera_on = false;
      if let Some(stream) = encoded.maybe_camera.take() {
        call.unpublish_video(stream);
      }
    },
    Control::WatchEncodedVideo(stream) => call.subscribe_video(*stream, Some(0)),
    Control::UnwatchEncodedVideo(stream) => call.subscribe_video(*stream, None),
    _ => {},
  }
}

fn parse_stream(text: &str) -> Option<MediaStreamId> {
  text.parse().ok().map(MediaStreamId)
}

fn track_info(track: &VideoTrack) -> VideoTrackInfo {
  VideoTrackInfo {
    stream_id: track.stream_id.0,
    device_id: track.device_id.to_string(),
    is_screen: track.kind == VideoTrackKind::Screen,
  }
}

/// Camera frames arrive upright, so a portrait phone sends portrait video; I420 needs even sides.
fn upright(width: u16, height: u16) -> VideoResolution {
  VideoResolution {
    width: width & !1,
    height: height & !1,
  }
}

/// Updates the call's state; returns why the call ends if the event ends it. `reconnecting`:
/// other devices that are rejoining.
fn apply_event(
  state: &mut CallStateInfo,
  reconnecting: &mut BTreeSet<DeviceId>,
  event: CallEngineEvent,
) -> Option<CallEndReason> {
  match event {
    CallEngineEvent::Joined { participants, .. } => {
      state.phase = CallPhase::Connected;
      state.participant_count = count(participants.len());
    },
    CallEngineEvent::ParticipantJoined(_) => state.participant_count += 1,
    CallEngineEvent::ParticipantLeft(device_id) => {
      state.participant_count = state.participant_count.saturating_sub(1);
      reconnecting.remove(&device_id);
    },
    CallEngineEvent::Reconnecting => {
      state.phase = CallPhase::Reconnecting;
      // Who else is away comes again with the SFU's answer to the rejoin.
      reconnecting.clear();
    },
    CallEngineEvent::Reconnected => state.phase = CallPhase::Connected,
    CallEngineEvent::ParticipantReconnecting(device_id) => {
      reconnecting.insert(device_id);
    },
    CallEngineEvent::ParticipantReconnected(device_id) => {
      reconnecting.remove(&device_id);
    },
    CallEngineEvent::Secured { privacy_code, .. } => {
      state.is_secured = true;
      state.maybe_privacy_code = Some(privacy_code);
    },
    CallEngineEvent::MuteChanged(is_muted) => state.is_muted = is_muted,
    CallEngineEvent::DeafenChanged(is_deafened) => state.is_deafened = is_deafened,
    CallEngineEvent::RemovedByHost { .. } => return Some(CallEndReason::RemovedByHost),
    CallEngineEvent::EndedByHost { .. } => return Some(CallEndReason::EndedByHost),
    // Joined from somewhere else as this device: this side just leaves.
    CallEngineEvent::ConnectionReplaced => return Some(CallEndReason::Left),
    CallEngineEvent::Error(error) => warn!(error_code = ?error.error_code, "the SFU rejected a call request"),
    _ => {},
  }
  state.reconnecting_count = count(reconnecting.len());
  None
}

fn ended(conversation: ConversationId, reason: CallEndReason) -> CallStateInfo {
  CallStateInfo {
    conversation_id: conversation.to_string(),
    phase: CallPhase::Ended,
    is_muted: false,
    is_deafened: false,
    is_camera_on: false,
    is_secured: false,
    participant_count: 0,
    reconnecting_count: 0,
    maybe_privacy_code: None,
    maybe_end_reason: Some(reason),
  }
}

/// The phone's default microphone and output (Telecom routes them); H.264 video, sent from the
/// app's camera frames and received once Cisco's OpenH264 is installed for decoding.
fn native_config(preparation: CallPreparation) -> Result<NativeCallConfig, TeamCraftError> {
  let video_decoders = if openh264_library::is_available() {
    vec![VideoCodec::H264]
  } else {
    Vec::new()
  };
  let engine = CallEngineConfig {
    user_id: preparation.user_id,
    device_id: preparation.device_id,
    capabilities: MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders,
      max_simulcast_layers: CAMERA_LAYERS,
    },
    bwe: BweConfig::default(),
  };
  let authenticator =
    DeviceCallKeyAuthenticator::from_verified(Arc::new(CoreSigner(preparation.signer)), preparation.devices)
      .map_err(|error| TeamCraftError::Call(error.to_string()))?;
  let mut config = NativeCallConfig::device_bound(
    preparation.join.sfu_url,
    preparation.join.token,
    engine,
    authenticator,
  );
  config.source = AudioSource::Microphone(AudioDeviceChoice::Default);
  config.sink = AudioSink::Speakers(AudioDeviceChoice::Default);
  config.maybe_processing = Some(AudioProcessingConfig::default());
  Ok(config)
}

fn status_cues(sounds: CallStatusSounds) -> StatusCues {
  match sounds {
    CallStatusSounds::All => StatusCues::All,
    CallStatusSounds::OwnOnly => StatusCues::OwnOnly,
    CallStatusSounds::Off => StatusCues::Off,
  }
}

fn count(len: usize) -> u32 {
  u32::try_from(len).unwrap_or(u32::MAX)
}

impl JoinTicketSource for EngineJoinTickets {
  fn fetch(&self) -> TicketFuture {
    let (reply, answer) = oneshot::channel();
    let conversation = self.conversation;
    let job: Job = Box::new(move |engine| {
      Box::pin(async move {
        // The call thread may have given up waiting.
        let _ = reply.send(engine.call_join_token(conversation).await);
      })
    });
    let is_sent = self.jobs.send(job).is_ok();
    Box::pin(async move {
      let stopped = || JoinTicketError {
        reason: TeamCraftError::EngineStopped.to_string(),
      };
      if !is_sent {
        return Err(stopped());
      }
      let answered: Result<CallJoinTokenResponse, TeamCraftError> = answer.await.map_err(|_| stopped())?;
      let join = answered.map_err(|error| JoinTicketError {
        reason: error.to_string(),
      })?;
      Ok(JoinTicket {
        sfu_url: join.sfu_url,
        token: join.token,
      })
    })
  }
}

impl CallBindingSigner for CoreSigner {
  fn sign_call_key_binding(
    &self,
    call_id: CryptoCallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError> {
    self.0.sign_call_key_binding(call_id, dave_public_key, not_after)
  }
}

fn lock_listener(listener: &SharedVideoListener) -> MutexGuard<'_, Option<Arc<dyn VideoListener>>> {
  // A single assignment per update; a poisoned lock holds a consistent value.
  listener.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock(state: &Mutex<HubState>) -> MutexGuard<'_, HubState> {
  // A panicking listener can't leave the hub inconsistent: each update is a single assignment.
  state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CallId, Id, Snowflake, VideoTrack};

  use super::*;

  #[test]
  fn rings_reach_the_listener_unless_in_that_call() {
    let hub = CallHub::default();
    let listener = Arc::new(RecordingListener::default());
    hub.set_listener(listener.clone());

    hub.on_gateway_event(&ring(5));
    lock(&hub.inner).maybe_active = Some(conversation(6));
    hub.on_gateway_event(&ring(6));
    hub.on_gateway_event(&GatewayEvent::CallEnd {
      conversation_id: conversation(5),
      call_id: call(1),
    });

    assert_eq!(*listener.incoming.lock().unwrap(), vec!["5".to_string()]);
    assert_eq!(*listener.ring_ended.lock().unwrap(), vec!["5".to_string()]);
  }

  #[test]
  fn engine_events_update_the_state() {
    let mut state = ended(conversation(5), CallEndReason::Left);
    let mut reconnecting = BTreeSet::new();
    state.phase = CallPhase::Connecting;
    assert_eq!(
      apply_event(&mut state, &mut reconnecting, CallEngineEvent::MuteChanged(true)),
      None
    );
    assert!(state.is_muted);
    assert_eq!(
      apply_event(
        &mut state,
        &mut reconnecting,
        CallEngineEvent::EndedByHost { by_user_id: user(1) }
      ),
      Some(CallEndReason::EndedByHost)
    );
  }

  #[test]
  fn reconnecting_is_a_phase_not_an_end() {
    let mut state = ended(conversation(5), CallEndReason::Left);
    let mut reconnecting = BTreeSet::new();
    state.phase = CallPhase::Connected;
    for event in [
      CallEngineEvent::ParticipantReconnecting(device(2)),
      CallEngineEvent::ParticipantReconnecting(device(3)),
      CallEngineEvent::ParticipantReconnected(device(2)),
    ] {
      assert_eq!(apply_event(&mut state, &mut reconnecting, event), None);
    }
    assert_eq!(state.reconnecting_count, 1);
    assert_eq!(
      apply_event(&mut state, &mut reconnecting, CallEngineEvent::Reconnecting),
      None
    );
    assert_eq!(state.phase, CallPhase::Reconnecting);
    assert_eq!(state.reconnecting_count, 0);
    apply_event(&mut state, &mut reconnecting, CallEngineEvent::Reconnected);
    assert_eq!(state.phase, CallPhase::Connected);
  }

  #[test]
  fn a_late_encoded_video_listener_learns_about_published_tracks() {
    let mut video = EncodedVideoBridge::default();
    let own = device(1);
    let track = |stream: u32, device_raw: i64| VideoTrack {
      stream_id: MediaStreamId(stream),
      device_id: device(device_raw),
      kind: VideoTrackKind::Camera,
      codec: VideoCodec::H264,
      layers: Vec::new(),
    };
    video.report(own, &CallEngineEvent::VideoPublished(track(7, 2)));
    video.report(own, &CallEngineEvent::VideoPublished(track(8, 3)));
    video.report(own, &CallEngineEvent::VideoUnpublished(MediaStreamId(7)));
    // Own camera: remembered for sending, not shown as a remote track.
    video.is_camera_requested = true;
    video.report(own, &CallEngineEvent::VideoPublished(track(9, 1)));
    assert_eq!(video.maybe_camera, Some(MediaStreamId(9)));

    let listener = Arc::new(VideoRecorder::default());
    video.set_listener(listener.clone());
    assert_eq!(*listener.tracks.lock().unwrap(), vec!["8".to_string()]);
    assert!(video.is_active());
  }

  #[test]
  fn hold_and_mute_silence_the_microphone_together() {
    let mut audio = AudioControls::default();
    assert!(!audio.is_silent());
    audio.is_held = true;
    assert!(audio.is_silent());
    audio.is_held = false;
    audio.is_muted = true;
    assert!(audio.is_silent());
  }

  #[derive(Default)]
  struct VideoRecorder {
    tracks: Mutex<Vec<String>>,
  }

  impl EncodedVideoListener for VideoRecorder {
    fn on_remote_video_track(&self, track: RemoteVideoTrack) {
      self.tracks.lock().unwrap().push(track.stream_id);
    }

    fn on_remote_video_ended(&self, _stream_id: String) {}

    fn on_remote_video_frame(&self, _frame: RemoteVideoFrame) {}

    fn on_keyframe_requested(&self) {}
  }

  fn device(raw: i64) -> DeviceId {
    DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  #[derive(Default)]
  struct RecordingListener {
    incoming: Mutex<Vec<String>>,
    ring_ended: Mutex<Vec<String>>,
  }

  impl CallListener for RecordingListener {
    fn on_call_state(&self, _state: CallStateInfo) {}

    fn on_incoming_call(&self, call: IncomingCallInfo) {
      self.incoming.lock().unwrap().push(call.conversation_id);
    }

    fn on_ring_ended(&self, conversation_id: String) {
      self.ring_ended.lock().unwrap().push(conversation_id);
    }
  }

  fn ring(conversation_id: i64) -> GatewayEvent {
    GatewayEvent::CallRing {
      conversation_id: conversation(conversation_id),
      call_id: call(1),
      from_user_id: user(2),
    }
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn call(raw: i64) -> CallId {
    CallId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
