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

mod codec_library;
pub mod commands;
pub(crate) mod push_to_talk;
pub(crate) mod video;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use teamcraft_client_api::call_view::CallView;
use teamcraft_client_api::calls::{
  ActiveCall, BackgroundEffect as UiBackgroundEffect, CallEndReason, CallRequest, CallStatusSounds,
  MediaDevice, MediaDevices, PushToTalk, ScreenShareSound,
};
use teamcraft_client_api::remote::CallSnapshot;
use teamcraft_core::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::audio_device::{list_input_devices, list_output_devices};
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
use teamcraft_media::effects::{BackgroundImage, EffectHandle};
use teamcraft_media::system_audio::SystemAudioCapture;
use teamcraft_media::video_pipeline::{VideoSendConfig, preferred_encoders};
use teamcraft_proto::{
  AudioCodec, ConversationId, DeviceId, GatewayEvent, MediaCapabilities, MediaStreamId, VideoCodec,
  VideoTrackKind,
};
use tokio::sync::{mpsc, oneshot};

use self::codec_library::CodecLibrary;
pub(crate) use self::video::BUNDLED_MODEL;
use crate::core::{CallPreparation, CoreRuntime};

pub use teamcraft_client_api::remote::CALL_CHANGED_EVENT;

/// Development SFUs use self-signed certificates; this variable pins one by its SHA-256 hash (hex,
/// colons allowed). Production SFUs have certificates the system trusts.
const CERT_HASH_VARIABLE: &str = "TEAMCRAFT_SFU_CERT_HASH";

/// The call state of the signed-in user, shared by the Tauri commands and the media task.
pub struct CallHost {
  state: Arc<Mutex<CallState>>,
  notify: Arc<dyn Fn() + Send + Sync>,
  /// The running media call, if any; a mutex across awaits so joins and leaves don't interleave.
  media: tokio::sync::Mutex<Option<MediaCall>>,
  video_sinks: VideoSinks,
  codecs: CodecLibrary,
  maybe_segmentation_model: Option<PathBuf>,
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
  maybe_camera: Option<String>,
  background: UiBackgroundEffect,
  /// The picture when `background` is one ([`UiBackgroundEffect::Custom`] or
  /// [`UiBackgroundEffect::Org`]).
  maybe_custom_background: Option<Arc<BackgroundImage>>,
  /// The workspace policy requires an organization background behind the camera image.
  is_background_required: bool,
  /// Changes the effect of the running camera without restarting it.
  maybe_effect_handle: Option<EffectHandle>,
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
  Leave,
}

/// Gets a watched track's decrypted frames (see `video::h264_sink` and `video::jpeg_sink`).
pub type VideoSink = Arc<dyn Fn(&ReceivedVideoFrame) + Send + Sync>;
type VideoSinks = Arc<Mutex<HashMap<MediaStreamId, VideoSink>>>;

/// Lets the media layer sign with the core's signer (the device key stays in the core).
struct CoreSigner(CallKeySigner);

/// Fresh join tokens from the core, for rejoining after a lost connection.
pub struct CoreJoinTickets {
  runtime: Arc<CoreRuntime>,
  conversation: ConversationId,
}

impl CallHost {
  /// `notify` tells the UI that the call state changed.
  pub fn new(notify: Arc<dyn Fn() + Send + Sync>, files: MediaFiles) -> Self {
    Self {
      state: Arc::new(Mutex::new(CallState::default())),
      notify,
      media: tokio::sync::Mutex::new(None),
      video_sinks: Arc::default(),
      codecs: CodecLibrary::new(files.codec_directory),
      maybe_segmentation_model: files.maybe_segmentation_model,
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
    let devices = MediaDevices {
      cameras: cameras
        .into_iter()
        .map(|camera| MediaDevice {
          id: camera.id,
          label: camera.name,
        })
        .collect(),
      microphones: inputs
        .map_err(|error| error.to_string())?
        .iter()
        .map(media_device)
        .collect(),
      speakers: outputs
        .map_err(|error| error.to_string())?
        .iter()
        .map(media_device)
        .collect(),
    };
    lock(&self.state).devices = devices.clone();
    self.changed();
    Ok(devices)
  }

  /// Joins the conversation's call with what the core prepared, leaving any other call first.
  pub async fn join(
    &self,
    request: CallRequest,
    preparation: CallPreparation,
    tickets: Arc<dyn JoinTicketSource>,
  ) -> Result<(), String> {
    let mut media = self.media.lock().await;
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
    let call = NativeCall::join(config)
      .await
      .map_err(|error| error.to_string())?;
    if self.push_to_talk().is_enabled {
      // Push-to-talk: the microphone opens only while the key is held.
      call.set_muted(true);
    }
    {
      let mut state = lock(&self.state);
      let background = state.background;
      state.view.start_active(request, background);
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
        video_sinks: Arc::clone(&self.video_sinks),
      },
    ));
    *media = Some(MediaCall { controls, task });
    self.changed();
    Ok(())
  }

  pub async fn leave(&self) {
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

  /// Receives another device's track and hands each frame to `sink` (the webview decodes it).
  pub async fn watch_video(&self, stream_id: MediaStreamId, maybe_max_layer: Option<u8>, sink: VideoSink) {
    lock(&self.video_sinks).insert(stream_id, sink);
    self
      .control(Control::Subscribe {
        stream_id,
        maybe_max_layer: Some(maybe_max_layer.unwrap_or(u8::MAX)),
      })
      .await;
  }

  pub async fn unwatch_video(&self, stream_id: MediaStreamId) {
    if lock(&self.video_sinks).remove(&stream_id).is_some() {
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
    self.switch_audio().await
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
      lock(&self.state).maybe_effect_handle = None;
      return Ok(());
    }
    let is_required = lock(&self.state).is_background_required;
    if is_required && !matches!(lock(&self.state).background, UiBackgroundEffect::Org(_)) {
      return Err("the organization requires one of its backgrounds for the camera".into());
    }
    self.ensure_video_codec().await?;
    let (maybe_camera, background, maybe_custom) = {
      let state = lock(&self.state);
      (
        state.maybe_camera.clone(),
        state.background,
        state.maybe_custom_background.clone(),
      )
    };
    let maybe_model = self.maybe_segmentation_model.clone();
    let (capture, maybe_effects) = tokio::task::spawn_blocking(move || {
      let capture = video::open_camera(maybe_camera.as_deref())?;
      Ok::<_, String>((
        capture,
        video::effects_filter(background, maybe_custom.as_ref(), maybe_model.as_deref()),
      ))
    })
    .await
    .map_err(|error| error.to_string())??;
    let (maybe_filter, maybe_handle) = match maybe_effects {
      Some((filter, handle)) => (Some(filter), Some(handle)),
      // Sending the room behind the person would break the organization's rule.
      None if is_required => {
        return Err(
          "the organization requires a background, but background effects aren't available on this device"
            .into(),
        );
      },
      None => (None, None),
    };
    let config = VideoSendConfig {
      kind: VideoTrackKind::Camera,
      layers: video::camera_layers(capture.format()),
      capture,
      encoders: preferred_encoders(),
      maybe_filter,
    };
    self.start_video(config).await?;
    lock(&self.state).maybe_effect_handle = maybe_handle;
    self.update_active(|active| active.is_camera_on = true);
    Ok(())
  }

  /// Remembers the camera and restarts a running one with it.
  pub async fn select_camera(&self, maybe_device: Option<String>) -> Result<(), String> {
    lock(&self.state).maybe_camera = maybe_device;
    if self.is_camera_on() {
      self.set_camera(true).await?;
    }
    Ok(())
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
    let maybe_handle = {
      let mut state = lock(&self.state);
      state.background = effect;
      state.is_background_required = is_required;
      state.maybe_custom_background = maybe_custom.clone();
      state.maybe_effect_handle.clone()
    };
    match maybe_handle {
      Some(handle) => handle.set(video::media_effect(effect, maybe_custom.as_ref())),
      None if effect != UiBackgroundEffect::None && self.is_camera_on() => {
        tracing::warn!("background effects are unavailable (no segmentation model or GPU)");
      },
      None => {},
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
  pub async fn start_screen_share(&self, has_sound: bool) -> Result<(), String> {
    self.ensure_video_codec().await?;
    let maybe_token = lock(&self.state).maybe_screen_token.clone();
    let (capture, maybe_new_token) =
      tokio::task::spawn_blocking(move || video::open_screen(maybe_token.as_deref()))
        .await
        .map_err(|error| error.to_string())??;
    let config = VideoSendConfig {
      kind: VideoTrackKind::Screen,
      layers: video::screen_layers(capture.format()),
      capture,
      encoders: preferred_encoders(),
      maybe_filter: None,
    };
    self.start_video(config).await?;
    if maybe_new_token.is_some() {
      lock(&self.state).maybe_screen_token = maybe_new_token;
    }
    self.update_active(|active| active.is_sharing_screen = true);
    let screen_sound = if has_sound {
      self.start_screen_audio().await
    } else {
      ScreenShareSound::Off
    };
    self.update_active(|active| active.screen_sound = screen_sound);
    Ok(())
  }

  pub async fn stop_screen_share(&self) {
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

/// Owns the call: applies controls and turns engine events into the UI's view of the call.
/// What the media task shares with the host.
struct MediaShared {
  own_device: DeviceId,
  state: Arc<Mutex<CallState>>,
  notify: Arc<dyn Fn() + Send + Sync>,
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
    video_sinks,
  } = shared;
  let mut roster = CallRoster::new(own_device);
  loop {
    tokio::select! {
      maybe_control = controls.recv() => match maybe_control {
        Some(Control::Mute(is_muted)) => call.set_muted(is_muted),
        Some(Control::Deafen(is_deafened)) => call.set_deafened(is_deafened),
        Some(Control::Audio { source, sink, done }) => {
          let result = call.set_audio(source, sink).await.map_err(|error| error.to_string());
          let _ = done.send(result);
        },
        Some(Control::StartVideo(config, done)) => {
          let result = call.start_video(config).await.map_err(|error| error.to_string());
          let _ = done.send(result);
        },
        Some(Control::StopVideo(kind)) => call.stop_video(kind),
        Some(Control::StartScreenAudio(capture, done)) => {
          let result = call.start_screen_audio(capture).await.map_err(|error| error.to_string());
          let _ = done.send(result);
        },
        Some(Control::StopScreenAudio) => call.stop_screen_audio(),
        Some(Control::Subscribe { stream_id, maybe_max_layer }) => call.subscribe_video(stream_id, maybe_max_layer),
        Some(Control::StatusSounds(sounds)) => call.set_status_cues(status_cues(sounds)),
        Some(Control::NetworkChanged) => call.network_changed(),
        Some(Control::Leave) | None => break,
      },
      maybe_event = call.next_event() => match maybe_event {
        Some(CallEngineEvent::VideoFrame(frame)) => {
          let maybe_sink = lock(&video_sinks).get(&frame.stream_id).cloned();
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
  // The native driver pins one certificate: the first the server names.
  let maybe_server_hash = preparation
    .join
    .sfu_certificate_sha256
    .first()
    .and_then(|hash| certificate_hash_from_hex(hash));
  let mut config = NativeCallConfig::device_bound(
    preparation.join.sfu_url,
    preparation.join.token,
    engine,
    authenticator,
  );
  // A development SFU's certificate: pinned here, or announced by the server with the token.
  config.maybe_certificate_hash = certificate_hash_from_env()?.or(maybe_server_hash);
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

  fn host() -> CallHost {
    CallHost::new(
      Arc::new(|| {}),
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
