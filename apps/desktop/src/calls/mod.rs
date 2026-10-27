//! Calls on the desktop: the server's call roster and ringing (from the core's gateway), and the
//! media of the call this device is in (`teamcraft-media`'s native driver: WebTransport to the SFU,
//! the microphone and speakers, Opus, DAVE).
//!
//! [`CallHost`] keeps what the UI shows as a [`CallSnapshot`] and emits
//! [`CALL_CHANGED_EVENT`] whenever it changes; the UI re-reads with `call_snapshot`. Joining asks the
//! core for a join token, the verified device certificates of the conversation and a signer for the
//! call key binding (e2ee.md §8), so DAVE only admits devices the user's contacts certified.
//!
//! A lost connection (a laptop changing networks) doesn't end the call: the driver migrates or
//! rejoins with a fresh token from the core, and the UI shows "reconnecting" meanwhile.
//!
//! Before a join, the pre-join screen checks the camera and microphone here (`preview`): the
//! camera through the call's effects into the self view, the microphone into a level meter.

mod codec_library;
pub mod commands;
mod preview;
pub(crate) mod push_to_talk;
mod self_view;
pub(crate) mod video;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_client_api::call_view::CallView;
use teamcraft_client_api::calls::{
  ActiveCall, BackgroundEffect as UiBackgroundEffect, CallEndReason, CallPreview, CallRequest,
  CallStatusSounds, CameraFallback, DevicePresence, MediaDevice, MediaDevices, PreviewCamera, PushToTalk,
  ScreenShareSound,
};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_client_api::remote::CallSnapshot;
use teamcraft_core::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::audio_device::{AudioDeviceError, list_input_devices, list_output_devices};
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource};
use teamcraft_media::audio_processing::AudioProcessingConfig;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallBindingSigner, DeviceCallKeyAuthenticator};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_native::{
  JoinTicketSource, NativeCall, NativeCallConfig, NativeCallError, TicketFuture,
};
use teamcraft_media::call_resilience::{JoinTicket, JoinTicketError, certificate_hash_from_hex};
use teamcraft_media::call_video::ReceivedVideoFrame;
use teamcraft_media::call_view::{CallRoster, status_cues};
use teamcraft_media::camera_choice::CameraReport;
use teamcraft_media::effects::{BackgroundImage, EffectHandle};
use teamcraft_media::ringtone;
use teamcraft_media::system_audio::SystemAudioCapture;
use teamcraft_media::video_capture::{CameraInfo, CaptureError};
use teamcraft_media::video_pipeline::{BlackReport, VideoSendConfig, VideoStartError, preferred_encoders};
use teamcraft_proto::{
  AudioCodec, ConversationId, DeviceId, GatewayEvent, MediaCapabilities, MediaStreamId, ScreenContentMode,
  VideoCodec, VideoTrackKind,
};
use tokio::sync::{mpsc, oneshot};

use self::codec_library::CodecLibrary;
use self::preview::{CameraCheck, LevelWatch, MicrophoneCheck, Preview};
use self::self_view::SelfView;
pub(crate) use self::video::BUNDLED_MODEL;
use self::video::OpenedCamera;
use crate::core::{CallPreparation, CoreRuntime};

pub use teamcraft_client_api::remote::CALL_CHANGED_EVENT;

/// Development SFUs use self-signed certificates; this variable pins one by its SHA-256 hash (hex,
/// colons allowed). Production SFUs have certificates the system trusts.
const CERT_HASH_VARIABLE: &str = "TEAMCRAFT_SFU_CERT_HASH";

/// The call state of the signed-in user, shared by the Tauri commands and the media task.
pub struct CallHost {
  state: Arc<Mutex<CallState>>,
  notify: Arc<dyn Fn() + Send + Sync>,
  /// Tells the UI about a failure no action waits for (`failure::ActionFailure` wire text).
  report_failure: FailureReporter,
  /// The running media call, if any; a mutex across awaits so joins and leaves don't interleave.
  media: tokio::sync::Mutex<Option<MediaCall>>,
  video_sinks: VideoSinks,
  /// The own camera for the webview's self view.
  self_view: SelfView,
  codecs: CodecLibrary,
  maybe_segmentation_model: Option<PathBuf>,
  /// The pre-join check (#442); a mutex across awaits, always taken after `media`.
  preview: tokio::sync::Mutex<Preview>,
  /// Where the pre-join check's microphone level goes.
  levels: LevelWatch,
  /// The screen picker a share start waits on, if one is open.
  screen_picker: OpenPicker,
}

/// The system's screen picker while a share start waits for the user's choice in it: stopping
/// the share or leaving the call ends the wait (and closes the picker where the system lets us),
/// and a second share start doesn't open another (issue #683: a picker nobody answered held the
/// call's other actions).
#[derive(Default)]
struct OpenPicker(Mutex<Option<Arc<tokio::sync::Notify>>>);

/// A share start's wait for the picker; marks the picker closed when dropped (the start answered,
/// failed or was given up).
struct PickerWait<'a> {
  picker: &'a OpenPicker,
  cancelled: Arc<tokio::sync::Notify>,
}

/// Files the calls need from the app's directories.
pub struct MediaFiles {
  /// Where Cisco's OpenH264 is cached (the app's data directory).
  pub codec_directory: PathBuf,
  /// The bundled segmentation model for background effects, if the app has its resources.
  pub maybe_segmentation_model: Option<PathBuf>,
}

#[derive(Default)]
struct CallState {
  /// Calls, ringing and the active call as the UI shows them.
  view: CallView,
  devices: MediaDevices,
  microphone: AudioDeviceChoice,
  speakers: AudioDeviceChoice,
  /// The chosen camera (`None`: the default, `camera_choice`'s order). A camera that stood in for
  /// the chosen one replaces it for the rest of the session, so the iPhone isn't asked again.
  maybe_camera: Option<String>,
  /// The camera that delivers, while the call's camera or the pre-join check runs.
  maybe_camera_in_use: Option<String>,
  /// The camera that stood in for the chosen one at the last start (#819).
  maybe_camera_fallback: Option<CameraFallback>,
  background: UiBackgroundEffect,
  /// The picture when `background` is one ([`UiBackgroundEffect::Custom`] or
  /// [`UiBackgroundEffect::Org`]).
  maybe_custom_background: Option<Arc<BackgroundImage>>,
  /// The workspace policy requires an organization background behind the camera image.
  is_background_required: bool,
  /// Changes the effect of the running camera without restarting it.
  maybe_effect_handle: Option<EffectHandle>,
  /// The same for the pre-join check's camera.
  maybe_preview_effect: Option<EffectHandle>,
  preview_camera: PreviewCamera,
  /// Lets the screen picker remember the last choice.
  maybe_screen_token: Option<String>,
  push_to_talk: PushToTalk,
  /// Which call status sounds play (the user's setting).
  status_sounds: CallStatusSounds,
}

/// Control of the task that owns the [`NativeCall`].
struct MediaCall {
  controls: mpsc::UnboundedSender<Control>,
  task: tokio::task::JoinHandle<()>,
}

enum Control {
  Mute(bool),
  Deafen(bool),
  MoveHere,
  Audio {
    source: AudioSource,
    sink: AudioSink,
    done: oneshot::Sender<Result<(), String>>,
  },
  StartVideo(VideoSendConfig, oneshot::Sender<Result<(), String>>),
  StopVideo(VideoTrackKind),
  /// Sends a screen share's sound from this capture; the answer says whether it started.
  StartScreenAudio(Box<dyn SystemAudioCapture>, oneshot::Sender<Result<(), String>>),
  StopScreenAudio,
  StatusSounds(CallStatusSounds),
  NetworkChanged,
  Subscribe {
    stream_id: MediaStreamId,
    maybe_max_layer: Option<u8>,
  },
  /// The webview's decoder of a watched track failed.
  RequestKeyframe(MediaStreamId),
  Leave,
}

/// Shows the UI a failure as `failure::ActionFailure` wire text.
pub type FailureReporter = Arc<dyn Fn(String) + Send + Sync>;

/// Gets a watched track's decrypted frames (see `video::h264_sink` and `video::jpeg_sink`).
pub type VideoSink = Arc<dyn Fn(&ReceivedVideoFrame) + Send + Sync>;
type VideoSinks = Arc<Mutex<HashMap<MediaStreamId, WatchedVideo>>>;

/// The webview channel a watch sends through (`tauri::ipc::Channel::id`). An unwatch names it,
/// so a late unwatch of a replaced channel leaves the newer watch of the same track alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelId(pub u32);

/// One watched track: where its frames go.
#[derive(Clone)]
struct WatchedVideo {
  channel: ChannelId,
  sink: VideoSink,
}

/// Lets the media layer sign with the core's signer (the device key stays in the core).
struct CoreSigner(CallKeySigner);

/// Fresh join tokens from the core, for rejoining after a lost connection.
pub struct CoreJoinTickets {
  runtime: Arc<CoreRuntime>,
  conversation: ConversationId,
}

impl CallHost {
  /// `notify` tells the UI that the call state changed; `report_failure` shows a failure that
  /// comes after the action that caused it returned (a camera that never sends a picture).
  pub fn new(
    notify: Arc<dyn Fn() + Send + Sync>,
    report_failure: FailureReporter,
    files: MediaFiles,
  ) -> Self {
    Self {
      state: Arc::new(Mutex::new(CallState::default())),
      notify,
      report_failure,
      media: tokio::sync::Mutex::new(None),
      video_sinks: Arc::default(),
      self_view: SelfView::default(),
      codecs: CodecLibrary::new(files.codec_directory),
      maybe_segmentation_model: files.maybe_segmentation_model,
      preview: tokio::sync::Mutex::default(),
      levels: LevelWatch::default(),
      screen_picker: OpenPicker::default(),
    }
  }

  /// Makes sure video can be encoded and decoded (downloads Cisco's OpenH264 on first use).
  pub async fn ensure_video_codec(&self) -> Result<(), String> {
    self.codecs.ensure().await
  }

  /// For the core runtime: applies the gateway's call events on whatever thread they arrive.
  pub fn gateway_handler(self: &Arc<Self>) -> Arc<dyn Fn(GatewayEvent) + Send + Sync> {
    let host = Arc::clone(self);
    Arc::new(move |event| host.on_gateway_event(event))
  }

  pub fn snapshot(&self) -> CallSnapshot {
    let state = lock(&self.state);
    CallSnapshot {
      maybe_active: state.view.maybe_active.clone(),
      incoming: state.view.incoming.values().copied().collect(),
      running: state.view.running.values().cloned().collect(),
      devices: state.devices.clone(),
      video_tracks: state.view.video_tracks.values().cloned().collect(),
      push_to_talk: state.push_to_talk.clone(),
      maybe_end_reason: state.view.maybe_end_reason,
      preview_camera: state.preview_camera,
      maybe_camera_fallback: state.maybe_camera_fallback.clone(),
    }
  }

  /// Lists microphones and speakers again (devices come and go).
  pub async fn refresh_devices(&self) -> Result<MediaDevices, String> {
    let (inputs, outputs) = tokio::task::spawn_blocking(|| (list_input_devices(), list_output_devices()))
      .await
      .map_err(|error| error.to_string())?;
    let cameras = tokio::task::spawn_blocking(video::list_cameras)
      .await
      .map_err(|error| error.to_string())?;
    let mut devices = MediaDevices::listed(
      cameras.iter().map(camera_device).collect(),
      inputs
        .map_err(|error| error.to_string())?
        .iter()
        .map(media_device)
        .collect(),
      outputs
        .map_err(|error| error.to_string())?
        .iter()
        .map(media_device)
        .collect(),
    );
    // A sandbox (Flatpak) may hide the V4L2 devices while the camera portal still offers a
    // camera, so an empty list doesn't mean there is none.
    if cfg!(target_os = "linux") && devices.cameras.is_empty() {
      devices.camera_presence = DevicePresence::Unknown;
    }
    lock(&self.state).devices = devices.clone();
    self.changed();
    Ok(devices)
  }

  /// Plays the test sound on the speakers (`None` or empty: the default). When the chosen ones
  /// can't be opened it plays on the default and still fails, so the user learns why; the error is
  /// `ActionFailure` wire text.
  pub async fn test_speakers(&self, maybe_device: Option<String>) -> Result<(), String> {
    let choice = device_choice(maybe_device.filter(|id| !id.is_empty()));
    tokio::task::spawn_blocking(move || play_test_sound(&choice))
      .await
      .map_err(|error| error.to_string())?
      .map_err(|failure| failure.to_wire())
  }

  /// Joins the conversation's call with what the core prepared, leaving any other call first.
  /// `is_muted`: the microphone stays muted from the first frame.
  pub async fn join(
    &self,
    request: CallRequest,
    is_muted: bool,
    preparation: CallPreparation,
    tickets: Arc<dyn JoinTicketSource>,
  ) -> Result<(), String> {
    let mut media = self.media.lock().await;
    // The call opens the camera and microphone itself.
    self.end_preview().await;
    if let Some(previous) = media.take() {
      previous.leave().await;
    }
    let (microphone, speakers) = {
      let state = lock(&self.state);
      (state.microphone.clone(), state.speakers.clone())
    };
    let own_device = preparation.device_id;
    let mut config = native_config(preparation, microphone, speakers)?;
    config.maybe_ticket_source = Some(tickets);
    config.status_cues = status_cues(lock(&self.state).status_sounds);
    // Push-to-talk: the microphone opens only while the key is held.
    let is_muted_at_start = is_muted || self.push_to_talk().is_enabled;
    config.is_muted_at_start = is_muted_at_start;
    let call = NativeCall::join(config)
      .await
      .map_err(|error| error.to_string())?;
    {
      let mut state = lock(&self.state);
      let background = state.background;
      state.view.start_active(request, background);
      if let Some(active) = state.view.maybe_active.as_mut() {
        active.is_muted = is_muted_at_start;
      }
    }
    let (controls, control_rx) = mpsc::unbounded_channel();
    lock(&self.video_sinks).clear();
    let task = tokio::spawn(run_media(
      call,
      control_rx,
      MediaShared {
        own_device,
        state: Arc::clone(&self.state),
        notify: Arc::clone(&self.notify),
        report_failure: Arc::clone(&self.report_failure),
        video_sinks: Arc::clone(&self.video_sinks),
      },
    ));
    *media = Some(MediaCall { controls, task });
    self.changed();
    Ok(())
  }

  pub async fn leave(&self) {
    self.screen_picker.cancel();
    if let Some(call) = self.media.lock().await.take() {
      call.leave().await;
    }
    lock(&self.state).view.end_active();
    lock(&self.video_sinks).clear();
    self.changed();
  }

  /// Push-to-talk on or off (the command layer registers the shortcut); with it on, the call is
  /// muted except while the key is held.
  pub async fn set_push_to_talk(&self, is_enabled: bool, shortcut: String, is_global: bool) {
    {
      let mut state = lock(&self.state);
      state.push_to_talk = PushToTalk {
        is_enabled,
        shortcut,
        is_global,
        is_talking: false,
      };
    }
    self.control(Control::Mute(is_enabled)).await;
    self.changed();
  }

  /// The push-to-talk key went down or up; ignored while push-to-talk is off.
  pub async fn set_talking(&self, is_talking: bool) {
    {
      let mut state = lock(&self.state);
      if !state.push_to_talk.is_enabled || state.push_to_talk.is_talking == is_talking {
        return;
      }
      state.push_to_talk.is_talking = is_talking;
    }
    self.control(Control::Mute(!is_talking)).await;
    self.changed();
  }

  fn push_to_talk(&self) -> PushToTalk {
    lock(&self.state).push_to_talk.clone()
  }

  /// Receives another device's track and hands each frame to `sink` (the webview decodes it),
  /// replacing an earlier watch of the track.
  pub async fn watch_video(
    &self,
    stream_id: MediaStreamId,
    channel: ChannelId,
    maybe_max_layer: Option<u8>,
    sink: VideoSink,
  ) {
    lock(&self.video_sinks).insert(stream_id, WatchedVideo { channel, sink });
    self
      .control(Control::Subscribe {
        stream_id,
        maybe_max_layer: Some(maybe_max_layer.unwrap_or(u8::MAX)),
      })
      .await;
  }

  /// Sends the own camera, as the call sends it, to `send` as JPEG images (see `self_view`);
  /// `send` answers whether the image was delivered.
  pub fn watch_camera(&self, channel: ChannelId, send: impl Fn(Vec<u8>) -> bool + Send + 'static) {
    self.self_view.watch(channel, send);
  }

  /// Stops the self view, if `channel` is the one it sends through.
  pub fn unwatch_camera(&self, channel: ChannelId) {
    self.self_view.unwatch(channel);
  }

  /// Sends the pre-join check's microphone level (0.0 to 1.0) to `send` about ten times a second
  /// while the check measures; `send` answers whether the reading was delivered.
  pub fn watch_level(&self, channel: ChannelId, send: impl Fn(f32) -> bool + Send + 'static) {
    self.levels.watch(channel, send);
  }

  /// Stops sending the level, if `channel` is the one it goes through.
  pub fn unwatch_level(&self, channel: ChannelId) {
    self.levels.unwatch(channel);
  }

  /// Runs the pre-join check as asked (#442). An input starts when it is asked for and stops when
  /// it no longer is. A running call has the camera and microphone; then there is nothing to
  /// check. A failure is `ActionFailure` wire text; the other input still runs.
  pub async fn set_preview(&self, wanted: CallPreview) -> Result<(), String> {
    // The same order as a join: the call's lock, then the check's.
    let media = self.media.lock().await;
    if media.is_some() {
      return Ok(());
    }
    let mut preview = self.preview.lock().await;
    let previous = std::mem::replace(&mut preview.wanted, wanted);
    let camera = if wanted.is_camera_on == previous.is_camera_on {
      Ok(())
    } else if wanted.is_camera_on {
      self.start_preview_camera(&mut preview).await
    } else {
      self.stop_preview_camera(&mut preview).await;
      Ok(())
    };
    let microphone = if wanted.is_mic_on == previous.is_mic_on {
      Ok(())
    } else if wanted.is_mic_on {
      self.start_preview_microphone(&mut preview).await
    } else {
      preview.stop_microphone().await;
      Ok(())
    };
    camera.and(microphone)
  }

  /// Asks for a keyframe of a watched track whose decoder in the webview failed.
  pub async fn request_keyframe(&self, stream_id: MediaStreamId) {
    self.control(Control::RequestKeyframe(stream_id)).await;
  }

  /// Stops receiving a track, if `channel` is the one it is sent through.
  pub async fn unwatch_video(&self, stream_id: MediaStreamId, channel: ChannelId) {
    let is_removed = {
      let mut sinks = lock(&self.video_sinks);
      let is_current = sinks
        .get(&stream_id)
        .is_some_and(|watched| watched.channel == channel);
      is_current && sinks.remove(&stream_id).is_some()
    };
    if is_removed {
      self
        .control(Control::Subscribe {
          stream_id,
          maybe_max_layer: None,
        })
        .await;
    }
  }

  /// Stops ringing on this device; the call goes on for the others.
  pub fn decline(&self, conversation: ConversationId) {
    lock(&self.state).view.decline(conversation);
    self.changed();
  }

  pub async fn set_muted(&self, is_muted: bool) {
    self.control(Control::Mute(is_muted)).await;
  }

  pub async fn set_deafened(&self, is_deafened: bool) {
    self.control(Control::Deafen(is_deafened)).await;
  }

  /// Takes the call over from the account's other devices (#505).
  pub async fn move_here(&self) {
    self.control(Control::MoveHere).await;
  }

  /// The network changed or the computer woke up (the same signal the gateway reconnects on): a
  /// running call checks that its connection followed, and rejoins otherwise.
  pub fn network_changed(self: &Arc<Self>) {
    let host = Arc::clone(self);
    tauri::async_runtime::spawn(async move { host.control(Control::NetworkChanged).await });
  }

  /// Remembers the status sounds setting for later calls and applies it to the running one.
  pub async fn set_status_sounds(&self, sounds: CallStatusSounds) {
    lock(&self.state).status_sounds = sounds;
    self.control(Control::StatusSounds(sounds)).await;
  }

  /// Remembers the microphone for the next call and switches the running one; `None` follows
  /// the system default.
  pub async fn select_microphone(&self, maybe_device: Option<String>) -> Result<(), String> {
    lock(&self.state).microphone = device_choice(maybe_device);
    self.switch_audio().await?;
    self.restart_preview_microphone().await
  }

  pub async fn select_speakers(&self, maybe_device: Option<String>) -> Result<(), String> {
    lock(&self.state).speakers = device_choice(maybe_device);
    self.switch_audio().await
  }

  async fn switch_audio(&self) -> Result<(), String> {
    let (source, sink) = {
      let state = lock(&self.state);
      (
        AudioSource::Microphone(state.microphone.clone()),
        AudioSink::Speakers(state.speakers.clone()),
      )
    };
    let media = self.media.lock().await;
    let Some(call) = media.as_ref() else {
      return Ok(());
    };
    let (done, done_rx) = oneshot::channel();
    if call.controls.send(Control::Audio { source, sink, done }).is_err() {
      return Ok(());
    }
    // No answer means the call ended meanwhile; the choice still applies to the next one.
    done_rx.await.unwrap_or(Ok(()))
  }

  /// Starts (opening the camera; the portal may ask the user) or stops the camera.
  pub async fn set_camera(&self, is_on: bool) -> Result<(), String> {
    if !is_on {
      self.control(Control::StopVideo(VideoTrackKind::Camera)).await;
      self.update_active(|active| active.is_camera_on = false);
      let mut state = lock(&self.state);
      state.maybe_effect_handle = None;
      state.maybe_camera_in_use = None;
      return Ok(());
    }
    self.ensure_video_codec().await?;
    let OpenedCamera {
      capture,
      report,
      maybe_filter,
      maybe_handle,
    } = self.open_camera().await?;
    let config = VideoSendConfig {
      kind: VideoTrackKind::Camera,
      layers: video::camera_layers(capture.format()),
      capture,
      encoders: preferred_encoders(),
      maybe_filter: Some(self.self_view.filter(maybe_filter)),
      maybe_idle_refresh: None,
    };
    // Answers once the capture runs; the media task turns the camera on in the call state, and
    // off again (with an error for the user) if no picture comes.
    self.start_video(config).await?;
    if self.is_camera_on() {
      lock(&self.state).maybe_effect_handle = maybe_handle;
    }
    self.note_camera_in_use(&report);
    self.changed();
    Ok(())
  }

  /// The background behind the camera, as last applied under the organization's rule.
  pub fn background(&self) -> UiBackgroundEffect {
    lock(&self.state).background
  }

  /// Remembers the camera and restarts a running one (or the pre-join check's) with it. The same
  /// choice again, or the camera that already delivers (the default the UI names, a camera that
  /// stood in), restarts nothing.
  pub async fn select_camera(&self, maybe_device: Option<String>) -> Result<(), String> {
    let maybe_device = maybe_device.filter(|id| !id.is_empty());
    {
      let mut state = lock(&self.state);
      if state.maybe_camera == maybe_device {
        return Ok(());
      }
      let is_in_use = maybe_device.is_some() && state.maybe_camera_in_use == maybe_device;
      state.maybe_camera = maybe_device;
      if is_in_use {
        return Ok(());
      }
    }
    if self.is_camera_on() {
      self.set_camera(true).await?;
    }
    self.restart_preview_camera().await
  }

  /// `maybe_custom`: the decoded picture for [`UiBackgroundEffect::Custom`] or
  /// [`UiBackgroundEffect::Org`] ([`video::decode_background`]). `effect` already follows the
  /// workspace rule ([`teamcraft_client_api::calls::OrgBackgrounds::effective`]); `is_required`
  /// makes the camera refuse to send without an organization background.
  pub fn set_background_effect(
    &self,
    effect: UiBackgroundEffect,
    maybe_custom: Option<Arc<BackgroundImage>>,
    is_required: bool,
  ) {
    let (maybe_handle, maybe_preview_handle) = {
      let mut state = lock(&self.state);
      state.background = effect;
      state.is_background_required = is_required;
      state.maybe_custom_background = maybe_custom.clone();
      (
        state.maybe_effect_handle.clone(),
        state.maybe_preview_effect.clone(),
      )
    };
    match maybe_handle {
      Some(handle) => handle.set(video::media_effect(effect, maybe_custom.as_ref())),
      None if effect != UiBackgroundEffect::None && self.is_camera_on() => {
        tracing::warn!("background effects are unavailable (no segmentation model or GPU)");
      },
      None => {},
    }
    // The pre-join check shows the choice at once, as the call will send it.
    if let Some(handle) = maybe_preview_handle {
      handle.set(video::media_effect(effect, maybe_custom.as_ref()));
    }
    self.update_active(|active| active.background = effect);
  }

  /// The organization requires a background but its picture can't be used: switches to strong
  /// blur, which still hides the room, and turns the camera off where there are no effects to
  /// apply it. The camera can't be turned on again until an organization picture loads.
  pub async fn fail_closed_background(&self) {
    tracing::warn!("required organization background unavailable; hiding the camera image");
    self.set_background_effect(UiBackgroundEffect::BlurStrong, None, true);
    let has_effects = lock(&self.state).maybe_effect_handle.is_some();
    if self.is_camera_on() && !has_effects {
      // Stopping only fails if the call is gone, and then nothing is sent anyway.
      let _ = self.set_camera(false).await;
    }
  }

  /// Shows the system's screen/window picker and shares the choice as its own stream, with the
  /// computer's sound when `has_sound`. Sound that can't be captured here doesn't stop the share;
  /// the call state says so.
  pub async fn start_screen_share(&self, has_sound: bool, mode: ScreenContentMode) -> Result<(), String> {
    self.ensure_video_codec().await?;
    let Some(wait) = self.screen_picker.open() else {
      tracing::info!("a screen picker is open already");
      return Ok(());
    };
    let maybe_token = lock(&self.state).maybe_screen_token.clone();
    let picking = tokio::task::spawn_blocking(move || video::open_screen(maybe_token.as_deref(), mode));
    let opened = tokio::select! {
      opened = picking => opened.map_err(|error| error.to_string())?,
      () = wait.cancelled.notified() => {
        // The picker's thread ends with the user's answer, which nobody takes any more.
        tracing::info!("screen share stopped while its picker was open");
        video::close_screen_picker();
        return Ok(());
      },
    };
    drop(wait);
    let (capture, maybe_new_token) = match opened {
      Ok(opened) => opened,
      // Closing the picker is a choice, not a failure: nothing is shared, nothing to report.
      Err(CaptureError::Cancelled(_)) => {
        tracing::info!("screen picker closed without a choice");
        return Ok(());
      },
      Err(error) => {
        tracing::warn!(%error, "screen share not started");
        return Err(video::screen_failure_wire(&error));
      },
    };
    let config = VideoSendConfig {
      kind: VideoTrackKind::Screen,
      layers: video::screen_layers(capture.format(), mode),
      capture,
      encoders: preferred_encoders(),
      maybe_filter: None,
      maybe_idle_refresh: VideoSendConfig::default_idle_refresh(&VideoTrackKind::Screen),
    };
    self.start_video(config).await?;
    if maybe_new_token.is_some() {
      lock(&self.state).maybe_screen_token = maybe_new_token;
    }
    self.changed();
    let is_sharing = lock(&self.state)
      .view
      .maybe_active
      .as_ref()
      .is_some_and(|active| active.is_sharing_screen);
    // The share may already have ended (no picture): its sound stays off then.
    let screen_sound = if has_sound && is_sharing {
      self.start_screen_audio().await
    } else {
      ScreenShareSound::Off
    };
    // A share that ended meanwhile keeps its sound shown as off.
    self.update_active(|active| {
      if active.is_sharing_screen {
        active.screen_sound = screen_sound;
      }
    });
    Ok(())
  }

  pub async fn stop_screen_share(&self) {
    self.screen_picker.cancel();
    self.control(Control::StopScreenAudio).await;
    self.control(Control::StopVideo(VideoTrackKind::Screen)).await;
    self.update_active(|active| {
      active.is_sharing_screen = false;
      active.screen_sound = ScreenShareSound::Off;
    });
  }

  /// Starts sending the computer's sound with the share.
  async fn start_screen_audio(&self) -> ScreenShareSound {
    let (done, done_rx) = oneshot::channel();
    {
      let media = self.media.lock().await;
      let Some(call) = media.as_ref() else {
        return ScreenShareSound::Off;
      };
      if call
        .controls
        .send(Control::StartScreenAudio(video::open_system_audio(), done))
        .is_err()
      {
        return ScreenShareSound::Off;
      }
    }
    match done_rx.await {
      Ok(Ok(())) => ScreenShareSound::Shared,
      Ok(Err(error)) => {
        tracing::warn!(%error, "the computer's sound can't be shared");
        ScreenShareSound::Unavailable
      },
      // The call ended meanwhile.
      Err(_) => ScreenShareSound::Off,
    }
  }

  /// Opens the chosen camera with the background effects in front of it, as the call sends it
  /// and the pre-join check shows it (blocking work on a blocking thread; the portal may ask).
  async fn open_camera(&self) -> Result<OpenedCamera, String> {
    // Which camera delivers is known again once this one started.
    lock(&self.state).maybe_camera_in_use = None;
    let is_required = lock(&self.state).is_background_required;
    if is_required && !matches!(lock(&self.state).background, UiBackgroundEffect::Org(_)) {
      return Err("the organization requires one of its backgrounds for the camera".into());
    }
    let (maybe_camera, background, maybe_custom) = {
      let state = lock(&self.state);
      (
        state.maybe_camera.clone(),
        state.background,
        state.maybe_custom_background.clone(),
      )
    };
    let maybe_model = self.maybe_segmentation_model.clone();
    let ((capture, report), maybe_effects) = tokio::task::spawn_blocking(move || {
      let opened = video::open_camera(maybe_camera.as_deref())?;
      Ok::<_, String>((
        opened,
        video::effects_filter(background, maybe_custom.as_ref(), maybe_model.as_deref()),
      ))
    })
    .await
    .map_err(|error| error.to_string())??;
    let (maybe_filter, maybe_handle) = match maybe_effects {
      Some((filter, handle)) => (Some(filter), Some(handle)),
      // Showing or sending the room behind the person would break the organization's rule.
      None if is_required => {
        return Err(
          "the organization requires a background, but background effects aren't available on this device"
            .into(),
        );
      },
      None => (None, None),
    };
    Ok(OpenedCamera {
      capture,
      report,
      maybe_filter,
      maybe_handle,
    })
  }

  /// Opens the camera for the pre-join check: effects, then the self view; nothing is encoded.
  async fn start_preview_camera(&self, preview: &mut Preview) -> Result<(), String> {
    self.set_preview_camera(PreviewCamera::Starting);
    let opened = match self.open_camera().await {
      Ok(opened) => opened,
      Err(error) => {
        self.set_preview_camera(PreviewCamera::Failed);
        return Err(error);
      },
    };
    let filter = self.self_view.filter(opened.maybe_filter);
    let (state, notify) = (Arc::clone(&self.state), Arc::clone(&self.notify));
    let on_state = move |camera| {
      lock(&state).preview_camera = camera;
      notify();
    };
    let report = opened.report.clone();
    let started = tokio::task::spawn_blocking(move || CameraCheck::start(opened.capture, filter, on_state))
      .await
      .map_err(|error| error.to_string())?;
    match started {
      Ok(check) => {
        preview.maybe_camera = Some(check);
        lock(&self.state).maybe_preview_effect = opened.maybe_handle;
        self.note_camera_in_use(&report);
        self.changed();
        Ok(())
      },
      // Already the UI's wire text.
      Err(error) => {
        self.set_preview_camera(PreviewCamera::Failed);
        Err(error)
      },
    }
  }

  /// Ends the check's camera and waits until it is released: a join may open it next.
  async fn stop_preview_camera(&self, preview: &mut Preview) {
    preview.stop_camera().await;
    {
      let mut state = lock(&self.state);
      state.maybe_preview_effect = None;
      state.maybe_camera_in_use = None;
    }
    self.set_preview_camera(PreviewCamera::Off);
  }

  /// Notes which camera delivers. One that stood in for the chosen camera (#819) becomes the
  /// choice for the rest of the session, so turning the camera off and on doesn't ask the iPhone
  /// again; the UI learns about the switch from the snapshot.
  fn note_camera_in_use(&self, report: &CameraReport) {
    let Some(used) = report.get() else {
      return;
    };
    let mut state = lock(&self.state);
    state.maybe_camera_in_use = Some(used.camera.id.clone());
    state.maybe_camera_fallback = None;
    if let Some(failed) = used.maybe_replaced {
      state.maybe_camera = Some(used.camera.id.clone());
      state.maybe_camera_fallback = Some(CameraFallback {
        used: camera_device(&used.camera),
        failed: camera_device(&failed),
      });
    }
  }

  async fn start_preview_microphone(&self, preview: &mut Preview) -> Result<(), String> {
    let (choice, levels) = (lock(&self.state).microphone.clone(), self.levels.clone());
    let check = tokio::task::spawn_blocking(move || MicrophoneCheck::start(choice, levels))
      .await
      .map_err(|error| error.to_string())??;
    preview.maybe_microphone = Some(check);
    Ok(())
  }

  /// Another camera was chosen: a running check shows it instead.
  async fn restart_preview_camera(&self) -> Result<(), String> {
    let media = self.media.lock().await;
    let mut preview = self.preview.lock().await;
    if media.is_some() || !preview.wanted.is_camera_on {
      return Ok(());
    }
    self.stop_preview_camera(&mut preview).await;
    self.start_preview_camera(&mut preview).await
  }

  /// Another microphone was chosen: a running check measures it instead.
  async fn restart_preview_microphone(&self) -> Result<(), String> {
    let media = self.media.lock().await;
    let mut preview = self.preview.lock().await;
    if media.is_some() || !preview.wanted.is_mic_on {
      return Ok(());
    }
    preview.stop_microphone().await;
    self.start_preview_microphone(&mut preview).await
  }

  /// Ends the pre-join check and waits until the camera and microphone are released.
  async fn end_preview(&self) {
    let mut preview = self.preview.lock().await;
    preview.wanted = CallPreview::default();
    self.stop_preview_camera(&mut preview).await;
    preview.stop_microphone().await;
  }

  fn set_preview_camera(&self, camera: PreviewCamera) {
    lock(&self.state).preview_camera = camera;
    self.changed();
  }

  async fn start_video(&self, config: VideoSendConfig) -> Result<(), String> {
    let (done, done_rx) = oneshot::channel();
    {
      let media = self.media.lock().await;
      let call = media.as_ref().ok_or("not in a call")?;
      call
        .controls
        .send(Control::StartVideo(config, done))
        .map_err(|_| "the call ended".to_owned())?;
    }
    done_rx.await.map_err(|_| "the call ended".to_owned())?
  }

  fn is_camera_on(&self) -> bool {
    lock(&self.state).view.is_camera_on()
  }

  fn update_active(&self, change: impl FnOnce(&mut ActiveCall)) {
    if let Some(active) = lock(&self.state).view.maybe_active.as_mut() {
      change(active);
    }
    self.changed();
  }

  async fn control(&self, control: Control) {
    if let Some(call) = self.media.lock().await.as_ref() {
      // A closed channel means the call just ended; nothing left to change.
      let _ = call.controls.send(control);
    }
  }

  fn on_gateway_event(&self, event: GatewayEvent) {
    if lock(&self.state).view.apply_gateway_event(&event) {
      self.changed();
    }
  }

  fn changed(&self) {
    (self.notify)();
  }
}

impl OpenPicker {
  /// Marks a picker open until the wait is dropped; `None` while another one is open.
  fn open(&self) -> Option<PickerWait<'_>> {
    let mut maybe_open = lock(&self.0);
    if maybe_open.is_some() {
      return None;
    }
    let cancelled = Arc::new(tokio::sync::Notify::new());
    *maybe_open = Some(Arc::clone(&cancelled));
    Some(PickerWait {
      picker: self,
      cancelled,
    })
  }

  /// Ends the wait of the open picker, if there is one. A permit is kept: a wait that starts
  /// right after still ends.
  fn cancel(&self) {
    if let Some(cancelled) = lock(&self.0).take() {
      cancelled.notify_one();
    }
  }
}

impl Drop for PickerWait<'_> {
  fn drop(&mut self) {
    let mut maybe_open = lock(&self.picker.0);
    // Only this wait's own mark: a cancel took it already, and another may have opened since.
    if maybe_open
      .as_ref()
      .is_some_and(|open| Arc::ptr_eq(open, &self.cancelled))
    {
      *maybe_open = None;
    }
  }
}

impl MediaCall {
  async fn leave(self) {
    let _ = self.controls.send(Control::Leave);
    if let Err(error) = self.task.await {
      tracing::warn!(%error, "call task failed");
    }
  }
}

impl CoreJoinTickets {
  pub fn new(runtime: Arc<CoreRuntime>, conversation: ConversationId) -> Self {
    Self {
      runtime,
      conversation,
    }
  }
}

impl JoinTicketSource for CoreJoinTickets {
  fn fetch(&self) -> TicketFuture {
    let (runtime, conversation) = (Arc::clone(&self.runtime), self.conversation);
    Box::pin(async move {
      let join = runtime
        .call_join_token(conversation)
        .await
        .map_err(|reason| JoinTicketError { reason })?;
      // With this token's certificate hashes: a restarted SFU may have a new self-signed one.
      Ok(JoinTicket::from(join))
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

/// Owns the call: applies controls and turns engine events into the UI's view of the call.
/// What the media task shares with the host.
struct MediaShared {
  own_device: DeviceId,
  state: Arc<Mutex<CallState>>,
  notify: Arc<dyn Fn() + Send + Sync>,
  report_failure: FailureReporter,
  video_sinks: VideoSinks,
}

async fn run_media(
  mut call: NativeCall,
  mut controls: mpsc::UnboundedReceiver<Control>,
  shared: MediaShared,
) {
  let MediaShared {
    own_device,
    state,
    notify,
    report_failure,
    video_sinks,
  } = shared;
  let mut roster = CallRoster::new(own_device);
  let mut video_starts = VideoStarts::default();
  let (failed, mut failed_rx) = mpsc::unbounded_channel::<VideoStartFailure>();
  let (black, mut black_rx) = mpsc::unbounded_channel::<VideoBlack>();
  loop {
    tokio::select! {
      maybe_control = controls.recv() => match maybe_control {
        Some(Control::Mute(is_muted)) => call.set_muted(is_muted),
        Some(Control::Deafen(is_deafened)) => call.set_deafened(is_deafened),
        Some(Control::MoveHere) => call.move_here(),
        Some(Control::Audio { source, sink, done }) => {
          let result = call.set_audio(source, sink).await.map_err(|error| error.to_string());
          let _ = done.send(result);
        },
        Some(Control::StartVideo(config, done)) => {
          let kind = config.kind.clone();
          let result = match call.start_video_checked(config).await {
            Ok(mut first_picture) => {
              // The answer doesn't wait for the first picture (up to seconds): other call actions
              // queue behind this one. The check reports later (`failed_rx`), and so does a track
              // that then shows nothing but black (`black_rx`, issue #683).
              let start = video_starts.started(&kind);
              set_video_flag(&state, &kind, true);
              let (failed, black) = (failed.clone(), black.clone());
              let maybe_black_pictures = first_picture.take_black_pictures();
              tokio::spawn(async move {
                if let Err(error) = first_picture.wait().await {
                  // The call ended meanwhile: nobody to tell.
                  let _ = failed.send(VideoStartFailure { start, error });
                  return;
                }
                let Some(black_pictures) = maybe_black_pictures else {
                  return;
                };
                if let Some(report) = black_pictures.wait().await {
                  let _ = black.send(VideoBlack { start, report });
                }
              });
              Ok(())
            },
            Err(error) => {
              tracing::warn!(%error, ?kind, "own video not started");
              Err(video::start_failure_wire(&kind, &error))
            },
          };
          // The caller may have given up waiting.
          let _ = done.send(result);
        },
        Some(Control::StopVideo(kind)) => {
          video_starts.stopped(&kind);
          call.stop_video(kind);
        },
        Some(Control::StartScreenAudio(capture, done)) => {
          // The share may have ended between its start and this (no picture came): its sound
          // must not go out on its own.
          let result = if video_starts.is_running(&VideoTrackKind::Screen) {
            call.start_screen_audio(capture).await.map_err(|error| error.to_string())
          } else {
            tracing::info!("screen share ended before its sound started; the sound stays off");
            Err("no screen is shared".to_owned())
          };
          let _ = done.send(result);
        },
        Some(Control::StopScreenAudio) => call.stop_screen_audio(),
        Some(Control::Subscribe { stream_id, maybe_max_layer }) => call.subscribe_video(stream_id, maybe_max_layer),
        Some(Control::RequestKeyframe(stream_id)) => call.request_keyframe(stream_id),
        Some(Control::StatusSounds(sounds)) => call.set_status_cues(status_cues(sounds)),
        Some(Control::NetworkChanged) => call.network_changed(),
        Some(Control::Leave) | None => break,
      },
      Some(failure) = failed_rx.recv() => {
        // A track stopped or replaced since ends without a picture too; that's no failure.
        if !video_starts.is_current(&failure.start) {
          continue;
        }
        let kind = failure.start.kind.clone();
        tracing::warn!(error = %failure.error, ?kind, "own video sends no picture; turning it off");
        video_starts.stopped(&kind);
        call.stop_video(kind.clone());
        if kind == VideoTrackKind::Screen {
          call.stop_screen_audio();
        }
        set_video_flag(&state, &kind, false);
        report_failure(video::start_failure_wire(&kind, &NativeCallError::from(failure.error)));
        notify();
      },
      Some(black) = black_rx.recv() => {
        // The track goes on (black may be what is shared); the user hears why it may be black.
        if video_starts.is_current(&black.start) {
          let kind = &black.start.kind;
          tracing::warn!(?kind, frames = black.report.frames, "own video shows nothing but black");
          report_failure(video::black_pictures_wire(kind, black.report));
        }
      },
      maybe_event = call.next_event() => match maybe_event {
        Some(CallEngineEvent::VideoFrame(frame)) => {
          let maybe_sink = lock(&video_sinks)
            .get(&frame.stream_id)
            .map(|watched| Arc::clone(&watched.sink));
          if let Some(sink) = maybe_sink {
            sink(&frame);
          }
        },
        Some(event) => {
          if roster.apply(&mut lock(&state).view, event) {
            notify();
          }
        },
        // The call ended on its own (the SFU closed it or the network failed).
        None => break,
      },
    }
  }
  match call.leave().await {
    Ok(stats) => tracing::info!(
      frames_sent = stats.frames_sent,
      frames_received = stats.frames_received,
      camera = ?stats.camera,
      screen = ?stats.screen,
      "left the call"
    ),
    Err(NativeCallError::ConnectionLost) => {
      tracing::warn!("the connection to the call was lost");
      lock(&state).view.maybe_end_reason = Some(CallEndReason::ConnectionLost);
    },
    Err(error) => tracing::warn!(%error, "call ended with an error"),
  }
  lock(&state).view.end_active();
  lock(&video_sinks).clear();
  notify();
}

/// Which start of each own track kind is the current one, so that the first-picture check of a
/// track stopped or replaced meanwhile doesn't turn its successor off.
#[derive(Debug, Default)]
struct VideoStarts {
  next: u64,
  current: HashMap<VideoTrackKind, u64>,
}

/// One start of an own track.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VideoStart {
  kind: VideoTrackKind,
  number: u64,
}

/// A started track whose first picture never came.
struct VideoStartFailure {
  start: VideoStart,
  error: VideoStartError,
}

/// A running track that showed nothing but black for a while.
struct VideoBlack {
  start: VideoStart,
  report: BlackReport,
}

impl VideoStarts {
  fn started(&mut self, kind: &VideoTrackKind) -> VideoStart {
    self.next += 1;
    self.current.insert(kind.clone(), self.next);
    VideoStart {
      kind: kind.clone(),
      number: self.next,
    }
  }

  fn stopped(&mut self, kind: &VideoTrackKind) {
    self.current.remove(kind);
  }

  fn is_current(&self, start: &VideoStart) -> bool {
    self.current.get(&start.kind) == Some(&start.number)
  }

  /// Whether a track of `kind` was started and is neither stopped nor failed.
  fn is_running(&self, kind: &VideoTrackKind) -> bool {
    self.current.contains_key(kind)
  }
}

/// Sets whether the own camera or screen share is on, as the UI shows it.
fn set_video_flag(state: &Mutex<CallState>, kind: &VideoTrackKind, is_on: bool) {
  let mut state = lock(state);
  if *kind == VideoTrackKind::Camera && !is_on {
    state.maybe_effect_handle = None;
    state.maybe_camera_in_use = None;
  }
  if let Some(active) = state.view.maybe_active.as_mut() {
    match kind {
      VideoTrackKind::Camera => active.is_camera_on = is_on,
      VideoTrackKind::Screen => {
        active.is_sharing_screen = is_on;
        if !is_on {
          active.screen_sound = ScreenShareSound::Off;
        }
      },
      _ => {},
    }
  }
}

/// The native call for a prepared join: device-bound DAVE keys, and only the conversation's
/// verified devices as the trusted roster.
fn native_config(
  preparation: CallPreparation,
  microphone: AudioDeviceChoice,
  speakers: AudioDeviceChoice,
) -> Result<NativeCallConfig, String> {
  let engine = CallEngineConfig {
    user_id: preparation.user_id,
    device_id: preparation.device_id,
    capabilities: MediaCapabilities {
      audio_codecs: vec![AudioCodec::Opus],
      video_encoders: vec![VideoCodec::H264],
      video_decoders: vec![VideoCodec::H264],
      max_simulcast_layers: 3,
    },
    bwe: BweConfig::default(),
  };
  let authenticator =
    DeviceCallKeyAuthenticator::from_verified(Arc::new(CoreSigner(preparation.signer)), preparation.devices)
      .map_err(|error| error.to_string())?;
  let ticket = JoinTicket::from(preparation.join);
  let mut config = NativeCallConfig::device_bound(ticket.sfu_url, ticket.token, engine, authenticator);
  // A development SFU's certificate: announced by the server with each token (rejoins use the
  // fresh token's), and one pinned here for every connection.
  config.certificate_hashes = ticket.certificate_hashes;
  config.maybe_certificate_hash = certificate_hash_from_env()?;
  config.source = AudioSource::Microphone(microphone);
  config.sink = AudioSink::Speakers(speakers);
  config.maybe_processing = Some(AudioProcessingConfig::default());
  Ok(config)
}

fn certificate_hash_from_env() -> Result<Option<[u8; 32]>, String> {
  let Ok(value) = std::env::var(CERT_HASH_VARIABLE) else {
    return Ok(None);
  };
  certificate_hash_from_hex(&value)
    .map(Some)
    .ok_or_else(|| format!("{CERT_HASH_VARIABLE} needs 32 bytes as hex"))
}

fn media_device(info: &AudioDeviceInfo) -> MediaDevice {
  MediaDevice {
    id: info.id.to_string(),
    label: info.name.clone(),
  }
}

fn camera_device(camera: &CameraInfo) -> MediaDevice {
  MediaDevice {
    id: camera.id.clone(),
    label: camera.name.clone(),
  }
}

fn play_test_sound(choice: &AudioDeviceChoice) -> Result<(), ActionFailure> {
  let samples = ringtone::test_sound();
  let missing = |error: AudioDeviceError| {
    ActionFailure::new(FailureKind::Call(CallFailure::SpeakersMissing), error.to_string())
  };
  match ringtone::play_once(choice, &samples) {
    Ok(()) => Ok(()),
    Err(error) if *choice != AudioDeviceChoice::Default => {
      tracing::warn!(%error, "the chosen speakers can't be opened; testing the default");
      ringtone::play_once(&AudioDeviceChoice::Default, &samples).map_err(missing)?;
      Err(ActionFailure::new(
        FailureKind::Call(CallFailure::SpeakersUnavailable),
        error.to_string(),
      ))
    },
    Err(error) => Err(missing(error)),
  }
}

fn device_choice(maybe_id: Option<String>) -> AudioDeviceChoice {
  match maybe_id {
    Some(id) => AudioDeviceChoice::Device(AudioDeviceId::new(id)),
    None => AudioDeviceChoice::Default,
  }
}

fn lock<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
  // Poisoning only means a panic mid-update of plain view data; keep going with it.
  state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CallId, Id, Snowflake, UserId};

  use super::*;

  #[test]
  fn gateway_ring_reaches_the_snapshot_until_declined() {
    let host = host();
    host.on_gateway_event(GatewayEvent::CallRing {
      conversation_id: conversation(5),
      call_id: CallId::from_snowflake(Snowflake::new(99).unwrap()),
      from_user_id: UserId::from_snowflake(Snowflake::new(1).unwrap()),
    });
    assert_eq!(host.snapshot().incoming.len(), 1);
    host.decline(conversation(5));
    assert!(host.snapshot().incoming.is_empty());
  }

  #[tokio::test]
  async fn push_to_talk_tracks_the_key_only_when_enabled() {
    let host = host();
    host.set_talking(true).await;
    assert!(!host.snapshot().push_to_talk.is_talking, "ignored while off");
    host.set_push_to_talk(true, "F8".into(), false).await;
    host.set_talking(true).await;
    let push_to_talk = host.snapshot().push_to_talk;
    assert!(push_to_talk.is_enabled && push_to_talk.is_talking);
    assert_eq!(push_to_talk.shortcut, "F8");
    host.set_talking(false).await;
    assert!(!host.snapshot().push_to_talk.is_talking);
  }

  #[tokio::test]
  async fn an_unusable_required_picture_fails_closed() {
    let host = host();
    host.set_background_effect(UiBackgroundEffect::None, None, false);
    host.fail_closed_background().await;
    let snapshot_background = lock(&host.state).background;
    assert_eq!(snapshot_background, UiBackgroundEffect::BlurStrong);
    assert!(lock(&host.state).is_background_required);
    // Blur alone doesn't satisfy the rule, so the camera stays off until a picture loads.
    assert!(host.set_camera(true).await.is_err());
  }

  #[tokio::test]
  async fn a_required_org_background_keeps_the_camera_off_without_one() {
    let host = host();
    host.set_background_effect(UiBackgroundEffect::BlurLight, None, true);
    let error = host.set_camera(true).await.unwrap_err();
    assert!(error.contains("requires one of its backgrounds"), "{error}");
    assert!(!host.is_camera_on());
  }

  /// Issue #683: a share start waiting in the picker must not hold the call.
  mod screen_picker {
    use std::time::Duration;

    use super::*;

    #[test]
    fn one_picker_at_a_time_until_its_wait_ends() {
      let picker = OpenPicker::default();
      let wait = picker.open().unwrap();
      assert!(
        picker.open().is_none(),
        "a second share start opens no second picker"
      );
      drop(wait);
      assert!(picker.open().is_some(), "the answered picker is closed");
    }

    #[tokio::test]
    async fn stopping_the_share_ends_the_wait_and_a_new_picker_may_open() {
      let picker = OpenPicker::default();
      let first = picker.open().unwrap();
      picker.cancel();
      // The permit is kept: the wait ends even though it starts after the cancel.
      tokio::time::timeout(Duration::from_secs(1), first.cancelled.notified())
        .await
        .unwrap();
      let second = picker.open().unwrap();
      // The cancelled wait going away leaves the new picker's mark alone.
      drop(first);
      assert!(picker.open().is_none());
      drop(second);
      picker.cancel();
      assert!(
        picker.open().is_some(),
        "cancelling without a picker changes nothing"
      );
    }
  }

  mod video_starts {
    use super::*;

    #[test]
    fn only_the_current_start_of_a_kind_counts() {
      let mut starts = VideoStarts::default();
      let first = starts.started(&VideoTrackKind::Camera);
      assert!(starts.is_current(&first));
      // Restarted (another camera): the first start's check no longer applies.
      let second = starts.started(&VideoTrackKind::Camera);
      assert!(!starts.is_current(&first));
      assert!(starts.is_current(&second));
      let screen = starts.started(&VideoTrackKind::Screen);
      assert!(
        starts.is_current(&second) && starts.is_current(&screen),
        "kinds are separate"
      );
      // Turned off: its check ending without a picture is no failure.
      starts.stopped(&VideoTrackKind::Camera);
      assert!(!starts.is_current(&second));
      assert!(starts.is_current(&screen));
      assert!(!starts.is_running(&VideoTrackKind::Camera));
      assert!(starts.is_running(&VideoTrackKind::Screen));
    }
  }

  /// The media task in a real call against a local SFU: what the UI's action queue waits for.
  mod media_task {
    use std::net::{Ipv6Addr, SocketAddr};
    use std::time::Duration;

    use teamcraft_client_api::calls::{CallKind, CallRequest};
    use teamcraft_media::video::{VideoResolution, simulcast_layers};
    use teamcraft_media::video_capture::{CaptureFormat, FrameSender, ManualCapture, VideoCapture};
    use teamcraft_media::video_pipeline::software_encoders;
    use teamcraft_sfu::auth::DevTokenVerifier;
    use teamcraft_sfu::config::SfuConfig;
    use teamcraft_sfu::server::{CALL_PATH, Sfu};

    use super::*;

    const FORMAT: CaptureFormat = CaptureFormat {
      resolution: VideoResolution {
        width: 320,
        height: 180,
      },
      framerate: 30,
    };
    /// Far below the first-picture check (10 s): an answer this fast didn't wait for it.
    const PROMPT: Duration = Duration::from_secs(3);

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn call_actions_do_not_wait_for_a_cameras_first_picture() {
      let mut media = start_media().await;
      // A camera that opens but never delivers: its check would take 10 s to fail.
      let answer = start_video(
        &media,
        VideoTrackKind::Camera,
        Box::new(ManualCapture::new(FORMAT)),
      );
      assert_eq!(
        tokio::time::timeout(PROMPT, answer).await.unwrap().unwrap(),
        Ok(())
      );
      assert!(media.flags().0, "the camera shows as on while its check runs");
      // Mute, a screen share and leaving go through at once.
      media.controls.send(Control::Mute(true)).unwrap();
      let screen = start_video(
        &media,
        VideoTrackKind::Screen,
        Box::new(ManualCapture::new(FORMAT)),
      );
      assert_eq!(
        tokio::time::timeout(PROMPT, screen).await.unwrap().unwrap(),
        Ok(())
      );
      media.controls.send(Control::Leave).unwrap();
      tokio::time::timeout(PROMPT, media.task).await.unwrap().unwrap();
      assert!(media.reported.try_recv().is_err(), "nothing failed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_camera_without_pictures_turns_off_and_says_so() {
      let mut media = start_media().await;
      let answer = start_video(&media, VideoTrackKind::Camera, Box::new(EndingCapture));
      assert_eq!(
        tokio::time::timeout(PROMPT, answer).await.unwrap().unwrap(),
        Ok(())
      );
      // The capture ends before its first picture: the check fails at once, after the answer.
      let reported = tokio::time::timeout(PROMPT, media.reported.recv())
        .await
        .unwrap()
        .unwrap();
      assert_eq!(
        ActionFailure::from_wire(&reported).kind,
        FailureKind::Call(CallFailure::CameraNoPicture)
      );
      assert!(!media.flags().0, "the camera is off again");
      media.controls.send(Control::Leave).unwrap();
      tokio::time::timeout(PROMPT, media.task).await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_screen_share_does_not_send_its_sound_alone() {
      let mut media = start_media().await;
      let answer = start_video(&media, VideoTrackKind::Screen, Box::new(EndingCapture));
      assert_eq!(
        tokio::time::timeout(PROMPT, answer).await.unwrap().unwrap(),
        Ok(())
      );
      // The share fails before its sound is asked for (the UI asks right after the answer).
      tokio::time::timeout(PROMPT, media.reported.recv())
        .await
        .unwrap()
        .unwrap();
      let (done, sound) = oneshot::channel();
      let capture = Box::new(teamcraft_media::system_audio::ManualSystemAudio::default());
      media
        .controls
        .send(Control::StartScreenAudio(capture, done))
        .unwrap();
      let result = tokio::time::timeout(PROMPT, sound).await.unwrap().unwrap();
      assert!(result.is_err(), "the computer's sound went out without a share");
      media.controls.send(Control::Leave).unwrap();
      tokio::time::timeout(PROMPT, media.task).await.unwrap().unwrap();
    }

    /// Issue #683: a share that macOS blanks (the permission not applied yet) tells the user why,
    /// and stays on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_black_screen_share_says_why_and_stays_on() {
      let mut media = start_media().await;
      let answer = start_video(&media, VideoTrackKind::Screen, Box::new(BlackCapture::default()));
      assert_eq!(
        tokio::time::timeout(PROMPT, answer).await.unwrap().unwrap(),
        Ok(())
      );
      let wait = teamcraft_media::video_pipeline::BLACK_PICTURES_REPORT_AFTER + PROMPT;
      let reported = tokio::time::timeout(wait, media.reported.recv())
        .await
        .unwrap()
        .unwrap();
      assert_eq!(
        ActionFailure::from_wire(&reported).kind,
        FailureKind::Call(CallFailure::ScreenBlack)
      );
      assert!(media.flags().1, "the share stays on");
      media.controls.send(Control::Leave).unwrap();
      tokio::time::timeout(PROMPT, media.task).await.unwrap().unwrap();
    }

    /// A running media task and what it shares.
    struct Media {
      controls: mpsc::UnboundedSender<Control>,
      task: tokio::task::JoinHandle<()>,
      state: Arc<Mutex<CallState>>,
      reported: mpsc::UnboundedReceiver<String>,
    }

    impl Media {
      /// Whether the camera and the screen share show as on.
      fn flags(&self) -> (bool, bool) {
        lock(&self.state)
          .view
          .maybe_active
          .as_ref()
          .map_or((false, false), |active| {
            (active.is_camera_on, active.is_sharing_screen)
          })
      }
    }

    /// A capture whose queue closes at once, like a camera that disappears.
    struct EndingCapture;

    impl VideoCapture for EndingCapture {
      fn format(&self) -> CaptureFormat {
        FORMAT
      }

      fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
        drop(frames);
        Ok(())
      }

      fn stop(&mut self) {}
    }

    /// A screen that shows one black picture and then stands still.
    #[derive(Default)]
    struct BlackCapture {
      maybe_frames: Option<FrameSender>,
    }

    impl VideoCapture for BlackCapture {
      fn format(&self) -> CaptureFormat {
        FORMAT
      }

      fn start(&mut self, frames: FrameSender) -> Result<(), CaptureError> {
        let resolution = FORMAT.resolution;
        let pixels = usize::from(resolution.width) * usize::from(resolution.height);
        let mut data = vec![16; pixels];
        data.resize(pixels * 3 / 2, 128);
        frames.send(teamcraft_media::video::VideoFrame::new(resolution, 0, data).unwrap());
        self.maybe_frames = Some(frames);
        Ok(())
      }

      fn stop(&mut self) {
        self.maybe_frames = None;
      }
    }

    async fn start_media() -> Media {
      let (port, certificate_hash) = start_sfu().await;
      let call = join(port, certificate_hash).await;
      let state = Arc::new(Mutex::new(CallState::default()));
      let request = CallRequest {
        conversation: conversation(5),
        kind: CallKind::Group,
      };
      lock(&state).view.start_active(request, UiBackgroundEffect::None);
      let (sink, reported) = mpsc::unbounded_channel();
      let (controls, control_rx) = mpsc::unbounded_channel();
      let shared = MediaShared {
        own_device: id(1),
        state: Arc::clone(&state),
        notify: Arc::new(|| {}),
        report_failure: Arc::new(move |failure| {
          // The test may be done already.
          let _ = sink.send(failure);
        }),
        video_sinks: Arc::default(),
      };
      let task = tokio::spawn(run_media(call, control_rx, shared));
      Media {
        controls,
        task,
        state,
        reported,
      }
    }

    fn start_video(
      media: &Media,
      kind: VideoTrackKind,
      capture: Box<dyn VideoCapture>,
    ) -> oneshot::Receiver<Result<(), String>> {
      let config = VideoSendConfig {
        maybe_idle_refresh: VideoSendConfig::default_idle_refresh(&kind),
        kind,
        capture,
        layers: simulcast_layers(FORMAT.resolution, 30, 300_000, 1),
        encoders: software_encoders(),
        maybe_filter: None,
      };
      let (done, answer) = oneshot::channel();
      media.controls.send(Control::StartVideo(config, done)).unwrap();
      answer
    }

    async fn join(port: u16, certificate_hash: [u8; 32]) -> NativeCall {
      let engine = CallEngineConfig {
        user_id: id(1),
        device_id: id(1),
        capabilities: MediaCapabilities {
          audio_codecs: vec![AudioCodec::Opus],
          video_encoders: vec![VideoCodec::H264],
          video_decoders: vec![VideoCodec::H264],
          max_simulcast_layers: 3,
        },
        bwe: BweConfig::default(),
      };
      let sfu_url = format!(
        "https://{}{CALL_PATH}",
        SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port)
      );
      let mut config = NativeCallConfig::new(sfu_url, "dev:14:1:1".to_owned(), engine);
      config.maybe_certificate_hash = Some(certificate_hash);
      NativeCall::join(config).await.unwrap()
    }

    async fn start_sfu() -> (u16, [u8; 32]) {
      let config = SfuConfig {
        bind_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        ..SfuConfig::default()
      };
      let identity = wtransport::Identity::self_signed(["localhost", "127.0.0.1", "::1"]).unwrap();
      let sfu = Sfu::bind(config, identity, Arc::new(DevTokenVerifier)).unwrap();
      let port = sfu.local_addr().unwrap().port();
      let certificate_hash = *sfu.certificate_hash().as_ref();
      tokio::spawn(sfu.run());
      (port, certificate_hash)
    }

    fn id<T: std::str::FromStr>(raw: i64) -> T
    where
      T::Err: std::fmt::Debug,
    {
      raw.to_string().parse().unwrap()
    }
  }

  fn host() -> CallHost {
    CallHost::new(
      Arc::new(|| {}),
      Arc::new(|_| {}),
      MediaFiles {
        codec_directory: std::env::temp_dir(),
        maybe_segmentation_model: None,
      },
    )
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
