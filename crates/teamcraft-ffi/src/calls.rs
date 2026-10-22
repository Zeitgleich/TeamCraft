//! Calls for the mobile apps: the engine thread prepares a join (token, device signer, verified
//! devices; e2ee.md §8) and the media runs in `teamcraft-media`'s native driver on a call thread of
//! its own. The app integrates the call with the platform (Android Telecom) and drives it through
//! [`CallSession`]; [`CallListener`] tells it about rings and the call's state.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use teamcraft_core::client::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_crypto::identity::VerifiedDevice;
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioSink, AudioSource};
use teamcraft_media::audio_processing::AudioProcessingConfig;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{NativeCall, NativeCallConfig};
use teamcraft_media::openh264_library;
use teamcraft_media::video::{VideoResolution, simulcast_layers};
use teamcraft_media::video_capture::CaptureFormat;
use teamcraft_media::video_pipeline::{VideoSendConfig, preferred_encoders};
use teamcraft_proto::{
  AudioCodec, CallJoinTokenResponse, ConversationId, DeviceId, GatewayEvent, MediaCapabilities,
  MediaStreamId, UserId, VideoCodec, VideoTrack, VideoTrackKind,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

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
  /// The code participants compare to rule out a man in the middle (once secured).
  pub maybe_privacy_code: Option<String>,
  /// Why the call ended (with [`CallPhase::Ended`]).
  pub maybe_end_reason: Option<CallEndReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallPhase {
  Connecting,
  Connected,
  Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum CallEndReason {
  Left,
  RemovedByHost,
  EndedByHost,
  /// The connection to the call broke.
  Failed,
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

  /// Leaves the call; the listener gets the ended state.
  pub fn leave(&self) {
    self.send(Control::Leave);
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
) -> Result<Arc<CallSession>, TeamCraftError> {
  let conversation = preparation.conversation;
  let own_device = preparation.device_id;
  let config = native_config(preparation)?;
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
  let mut video = RemoteVideo::default();
  loop {
    tokio::select! {
      maybe_control = controls.recv() => match maybe_control {
        // Leave asked for, or the app dropped the session.
        Some(Control::Leave) | None => break,
        Some(control) => {
          apply_control(&call, control, context, state, &video).await;
          context.hub.report(state.clone());
        },
      },
      maybe_event = call.next_event() => match maybe_event {
        Some(event) => {
          watch_video(&call, &event, context.own_device, &mut video);
          if let Some(end) = apply_event(state, event) {
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
  if let Err(error) = call.leave().await {
    info!(%error, "the call ended with an error");
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
    Control::Leave => {},
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

/// Updates the call's state; returns why the call ends if the event ends it.
fn apply_event(state: &mut CallStateInfo, event: CallEngineEvent) -> Option<CallEndReason> {
  match event {
    CallEngineEvent::Joined { participants, .. } => {
      state.phase = CallPhase::Connected;
      state.participant_count = count(participants.len());
    },
    CallEngineEvent::ParticipantJoined(_) => state.participant_count += 1,
    CallEngineEvent::ParticipantLeft(_) => {
      state.participant_count = state.participant_count.saturating_sub(1)
    },
    CallEngineEvent::Secured { privacy_code, .. } => {
      state.is_secured = true;
      state.maybe_privacy_code = Some(privacy_code);
    },
    CallEngineEvent::MuteChanged(is_muted) => state.is_muted = is_muted,
    CallEngineEvent::DeafenChanged(is_deafened) => state.is_deafened = is_deafened,
    CallEngineEvent::RemovedByHost { .. } => return Some(CallEndReason::RemovedByHost),
    CallEngineEvent::EndedByHost { .. } => return Some(CallEndReason::EndedByHost),
    CallEngineEvent::Error(error) => warn!(error_code = ?error.error_code, "the SFU rejected a call request"),
    _ => {},
  }
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

fn count(len: usize) -> u32 {
  u32::try_from(len).unwrap_or(u32::MAX)
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
  use teamcraft_proto::{CallId, Id, Snowflake};

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
    state.phase = CallPhase::Connecting;
    assert_eq!(apply_event(&mut state, CallEngineEvent::MuteChanged(true)), None);
    assert!(state.is_muted);
    assert_eq!(
      apply_event(&mut state, CallEngineEvent::EndedByHost { by_user_id: user(1) }),
      Some(CallEndReason::EndedByHost)
    );
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
