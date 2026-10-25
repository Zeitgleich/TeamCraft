//! The web client's call media (#330): a wasm module of its own that the page loads when a call
//! starts, so the call engine doesn't add to the app's download (robustness.md, "Download size").
//!
//! The page (`teamcraft-ui`, `web_calls`) keeps the call roster from the gateway, the device key
//! and the UI. This module runs `teamcraft-media`'s browser driver ([`WebCall`]: WebTransport,
//! WebCodecs, Web Audio, DAVE) and keeps the part of the call view the media layer knows
//! ([`MediaCallState`]), which it reports to the page after every change. The contract between the
//! two, and why it goes through JavaScript, is in `teamcraft_client_api::page_calls`.
#![cfg(target_arch = "wasm32")]

use std::cell::{Cell, RefCell};
use std::pin::pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use futures::future::{Either, select};
use js_sys::{Function, Promise, Reflect, Uint8Array};
use teamcraft_client_api::call_view::{CallView, MediaCallState};
use teamcraft_client_api::calls::{
  BackgroundEffect, CallEndReason, CallStatusSounds, MediaDevice, MediaDevices, ScreenShareSound,
};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_client_api::page_calls::{WORKLET_URL, WebCallJoin};
use teamcraft_client_api::remote::CallAction;
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallKeyAuthenticator, CallKeyError};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_resilience::{JoinTicket, JoinTicketError, ResilienceConfig};
use teamcraft_media::call_view::{CallRoster, status_cues};
use teamcraft_media::call_web::{
  self, LocalTicketFuture, MediaAccessReason, MediaInput, WebCall, WebCallConfig, WebCallError,
  WebJoinTicketSource,
};
use teamcraft_media::jitter::JitterBufferConfig;
use teamcraft_proto::{
  AudioCodec, CallId, CallJoinTokenResponse, DeviceId, MediaCapabilities, MediaStreamId, VideoCodec,
};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{HtmlCanvasElement, MediaStream, MediaStreamTrack};

/// Simulcast layers the camera sends (as on desktop).
const MAX_SIMULCAST_LAYERS: u8 = 3;

thread_local! {
  static HOST: RefCell<Option<Rc<Host>>> = const { RefCell::new(None) };
  static IS_LOGGING: Cell<bool> = const { Cell::new(false) };
}

/// The call of this page and what the user chose for the next one.
struct Host {
  callbacks: Callbacks,
  state: RefCell<State>,
  /// The running call, if any.
  maybe_media: RefCell<Option<Media>>,
}

/// The page's functions (see `page_calls`).
struct Callbacks {
  changed: Function,
  sign: Function,
  verify: Function,
  ticket: Function,
}

#[derive(Default)]
struct State {
  /// Only the media part (active call, video, end reason) is kept here; the page has the rest.
  view: CallView,
  microphone: AudioDeviceChoice,
  speakers: AudioDeviceChoice,
  maybe_camera: Option<String>,
  /// The stream the camera sends, shown as the self view.
  maybe_local_camera: Option<MediaStream>,
}

/// The task that owns the [`WebCall`].
struct Media {
  controls: mpsc::UnboundedSender<Control>,
  finished: oneshot::Receiver<()>,
}

type Done = oneshot::Sender<Result<(), WebCallError>>;

enum Control {
  Mute(bool),
  Deafen(bool),
  Audio {
    source: AudioSource,
    sink: AudioSink,
    done: Done,
  },
  Camera {
    stream: MediaStream,
    done: Done,
  },
  StopCamera,
  Screen {
    done: Done,
  },
  StopScreen,
  Watch {
    stream_id: MediaStreamId,
    canvas: HtmlCanvasElement,
  },
  Unwatch(MediaStreamId),
  StatusSounds(CallStatusSounds),
  Leave,
}

/// Signs and verifies call key bindings in the page, which holds the device key (e2ee.md §8).
#[derive(Debug)]
struct PageAuthenticator;

/// Fresh join tokens from the page's client core, for rejoining after a lost connection.
struct PageTickets;

/// Takes the page's callbacks; call once after the module is initialized.
#[wasm_bindgen]
pub fn start(callbacks: JsValue) -> Result<(), JsValue> {
  init_logging();
  let callbacks = Callbacks {
    changed: function(&callbacks, "changed")?,
    sign: function(&callbacks, "sign")?,
    verify: function(&callbacks, "verify")?,
    ticket: function(&callbacks, "ticket")?,
  };
  let host = Rc::new(Host {
    callbacks,
    state: RefCell::new(State::default()),
    maybe_media: RefCell::new(None),
  });
  HOST.with(|slot| *slot.borrow_mut() = Some(host));
  Ok(())
}

/// Joins a call (`WebCallJoin` JSON), leaving a running one first.
#[wasm_bindgen]
pub async fn join(join: String) -> Result<(), JsValue> {
  let host = host()?;
  let join: WebCallJoin = serde_json::from_str(&join)
    .map_err(|error| failure_value(ActionFailure::other(format!("unreadable join: {error}"))))?;
  host.join(join).await.map_err(failure_value)
}

/// Carries out a `CallAction` (JSON) other than joining.
#[wasm_bindgen]
pub async fn act(action: String) -> Result<(), JsValue> {
  let host = host()?;
  let action: CallAction = serde_json::from_str(&action)
    .map_err(|error| failure_value(ActionFailure::other(format!("unreadable call action: {error}"))))?;
  host.act(action).await.map_err(failure_value)
}

/// Microphones, speakers and cameras as `MediaDevices` JSON. Labels are empty until the page has
/// permission for the kind.
#[wasm_bindgen]
pub async fn devices() -> Result<String, JsValue> {
  let devices = call_web::list_media_devices()
    .await
    .map_err(|error| failure_value(failure(error)))?;
  let devices = MediaDevices {
    cameras: media_devices(&devices.cameras),
    microphones: media_devices(&devices.inputs),
    speakers: media_devices(&devices.outputs),
  };
  serde_json::to_string(&devices).map_err(|error| JsValue::from_str(&error.to_string()))
}

/// Draws another device's track (`MediaStreamId` JSON) into `canvas` until unwatched.
#[wasm_bindgen]
pub fn watch_video(stream_id: String, canvas: HtmlCanvasElement) {
  if let (Ok(host), Ok(stream_id)) = (host(), serde_json::from_str(&stream_id)) {
    host.control(Control::Watch { stream_id, canvas });
  }
}

#[wasm_bindgen]
pub fn unwatch_video(stream_id: String) {
  if let (Ok(host), Ok(stream_id)) = (host(), serde_json::from_str(&stream_id)) {
    host.control(Control::Unwatch(stream_id));
  }
}

/// The camera stream this device sends, for the self view.
#[wasm_bindgen]
pub fn local_camera() -> Option<MediaStream> {
  host().ok()?.state.borrow().maybe_local_camera.clone()
}

/// Leaves a running call and forgets the page's callbacks (the page signs out).
#[wasm_bindgen]
pub async fn stop() {
  let maybe_host = HOST.with(|slot| slot.borrow_mut().take());
  if let Some(host) = maybe_host {
    host.leave().await;
  }
}

impl Host {
  async fn join(self: &Rc<Self>, join: WebCallJoin) -> Result<(), ActionFailure> {
    self.leave().await;
    let (microphone, speakers) = {
      let mut state = self.state.borrow_mut();
      state.microphone = device_choice(join.maybe_microphone.clone());
      state.speakers = device_choice(join.maybe_speakers.clone());
      (state.microphone.clone(), state.speakers.clone())
    };
    let own_device = join.device_id;
    let config = WebCallConfig {
      sfu_url: join.sfu_url,
      maybe_certificate_hash: None,
      token: join.token,
      engine: CallEngineConfig {
        user_id: join.user_id,
        device_id: own_device,
        capabilities: MediaCapabilities {
          audio_codecs: vec![AudioCodec::Opus],
          video_encoders: vec![VideoCodec::H264],
          video_decoders: vec![VideoCodec::H264],
          max_simulcast_layers: MAX_SIMULCAST_LAYERS,
        },
        bwe: BweConfig::default(),
      },
      source: AudioSource::Microphone(microphone),
      sink: AudioSink::Speakers(speakers),
      maybe_trusted_roster: Some(join.trusted_devices),
      maybe_authenticator: Some(Arc::new(PageAuthenticator)),
      jitter: JitterBufferConfig::default(),
      resilience: ResilienceConfig::default(),
      maybe_ticket_source: Some(Rc::new(PageTickets)),
      status_cues: status_cues(join.status_sounds),
      maybe_worklet_url: Some(WORKLET_URL.to_owned()),
    };
    let call = WebCall::join(config).await.map_err(failure)?;
    self
      .state
      .borrow_mut()
      .view
      .start_active(join.request, BackgroundEffect::None);
    let (controls, control_rx) = mpsc::unbounded();
    let (finished_tx, finished) = oneshot::channel();
    *self.maybe_media.borrow_mut() = Some(Media { controls, finished });
    let host = Rc::downgrade(self);
    spawn_local(async move {
      run_media(call, control_rx, host, own_device).await;
      // Nobody waits when the call ended on its own.
      let _ = finished_tx.send(());
    });
    self.changed();
    Ok(())
  }

  async fn act(self: &Rc<Self>, action: CallAction) -> Result<(), ActionFailure> {
    match action {
      CallAction::Leave => self.leave().await,
      CallAction::SetMuted { is_muted } => self.control(Control::Mute(is_muted)),
      CallAction::SetDeafened { is_deafened } => self.control(Control::Deafen(is_deafened)),
      CallAction::SelectMicrophone { maybe_device } => {
        self.state.borrow_mut().microphone = device_choice(maybe_device);
        self.switch_audio().await?;
      },
      CallAction::SelectSpeakers { maybe_device } => {
        self.state.borrow_mut().speakers = device_choice(maybe_device);
        self.switch_audio().await?;
      },
      CallAction::SetCamera { is_on: true } => self.start_camera().await?,
      CallAction::SetCamera { is_on: false } => self.stop_camera(),
      CallAction::SelectCamera { maybe_device } => {
        self.state.borrow_mut().maybe_camera = maybe_device;
        if self.state.borrow().view.is_camera_on() {
          self.start_camera().await?;
        }
      },
      CallAction::StartScreenShare { has_sound } => self.start_screen_share(has_sound).await?,
      CallAction::StopScreenShare => {
        self.control(Control::StopScreen);
        self.update_active(|active| {
          active.is_sharing_screen = false;
          active.screen_sound = ScreenShareSound::Off;
        });
      },
      CallAction::SetStatusSounds { sounds } => self.control(Control::StatusSounds(sounds)),
      // The page answers these itself (ringing, push-to-talk) or the browser has none (effects).
      CallAction::Decline { .. }
      | CallAction::SetBackgroundEffect { .. }
      | CallAction::OpenSystemVideoEffects
      | CallAction::SetPushToTalk { .. }
      | CallAction::SetTalking { .. } => {},
      CallAction::Join { .. } | CallAction::Unknown => {
        return Err(ActionFailure::other("not a call module action"));
      },
    }
    Ok(())
  }

  async fn leave(&self) {
    let maybe_media = self.maybe_media.borrow_mut().take();
    if let Some(media) = maybe_media {
      // A closed channel means the call already ended; waiting still lets it finish.
      let _ = media.controls.unbounded_send(Control::Leave);
      let _ = media.finished.await;
    }
  }

  async fn switch_audio(&self) -> Result<(), ActionFailure> {
    let (source, sink) = {
      let state = self.state.borrow();
      (
        AudioSource::Microphone(state.microphone.clone()),
        AudioSink::Speakers(state.speakers.clone()),
      )
    };
    // Not in a call: the choice applies to the next one.
    self.ask(|done| Control::Audio { source, sink, done }).await
  }

  /// Opens the camera (the browser may ask) and sends it; replaces a running camera.
  async fn start_camera(&self) -> Result<(), ActionFailure> {
    if self.maybe_media.borrow().is_none() {
      return Err(ActionFailure::other("not in a call"));
    }
    let maybe_camera = self.state.borrow().maybe_camera.clone();
    let stream = call_web::capture_camera(maybe_camera.as_deref())
      .await
      .map_err(failure)?;
    let sent = stream.clone();
    if let Err(error) = self.ask(|done| Control::Camera { stream: sent, done }).await {
      stop_tracks(&stream);
      return Err(error);
    }
    self.state.borrow_mut().maybe_local_camera = Some(stream);
    self.update_active(|active| active.is_camera_on = true);
    Ok(())
  }

  fn stop_camera(&self) {
    self.control(Control::StopCamera);
    if let Some(stream) = self.state.borrow_mut().maybe_local_camera.take() {
      stop_tracks(&stream);
    }
    self.update_active(|active| active.is_camera_on = false);
  }

  /// Asks for a screen or window and shares it. Closing the browser's picker is not a failure.
  /// Browsers share no system sound with a call yet, so asked-for sound is reported unavailable.
  async fn start_screen_share(&self, has_sound: bool) -> Result<(), ActionFailure> {
    match self.ask_media(|done| Control::Screen { done }).await {
      Ok(()) => {},
      Err(WebCallError::MediaAccess {
        input: MediaInput::Screen,
        reason: MediaAccessReason::Denied,
        ..
      }) => return Ok(()),
      Err(error) => return Err(failure(error)),
    }
    self.update_active(|active| {
      active.is_sharing_screen = true;
      active.screen_sound = if has_sound {
        ScreenShareSound::Unavailable
      } else {
        ScreenShareSound::Off
      };
    });
    Ok(())
  }

  /// Sends `control` to the call task and waits for its answer; without a call there is none.
  async fn ask(&self, control: impl FnOnce(Done) -> Control) -> Result<(), ActionFailure> {
    self.ask_media(control).await.map_err(failure)
  }

  async fn ask_media(&self, control: impl FnOnce(Done) -> Control) -> Result<(), WebCallError> {
    let (done, answer) = oneshot::channel();
    let is_sent = self
      .maybe_media
      .borrow()
      .as_ref()
      .is_some_and(|media| media.controls.unbounded_send(control(done)).is_ok());
    if !is_sent {
      return Ok(());
    }
    // No answer: the call ended meanwhile.
    answer.await.unwrap_or(Ok(()))
  }

  fn control(&self, control: Control) {
    if let Some(media) = self.maybe_media.borrow().as_ref() {
      // A closed channel means the call just ended; nothing left to change.
      let _ = media.controls.unbounded_send(control);
    }
  }

  fn on_engine_event(&self, roster: &mut CallRoster, event: CallEngineEvent) {
    let is_changed = roster.apply(&mut self.state.borrow_mut().view, event);
    if is_changed {
      self.changed();
    }
  }

  /// The call task ended (left, or lost for good).
  fn finish(&self, result: Result<(), WebCallError>) {
    {
      let mut state = self.state.borrow_mut();
      if let Err(WebCallError::ConnectionLost) = result {
        state.view.maybe_end_reason = Some(CallEndReason::ConnectionLost);
      }
      state.view.end_active();
      if let Some(stream) = state.maybe_local_camera.take() {
        stop_tracks(&stream);
      }
    }
    if let Err(error) = result {
      tracing::warn!(%error, "the call ended with an error");
    }
    // Ended on its own: no media left to control.
    self.maybe_media.borrow_mut().take();
    self.changed();
  }

  fn update_active(&self, change: impl FnOnce(&mut teamcraft_client_api::calls::ActiveCall)) {
    if let Some(active) = self.state.borrow_mut().view.maybe_active.as_mut() {
      change(active);
    }
    self.changed();
  }

  fn changed(&self) {
    let state: MediaCallState = self.state.borrow().view.media_state();
    let Ok(json) = serde_json::to_string(&state) else {
      return;
    };
    if let Err(error) = self
      .callbacks
      .changed
      .call1(&JsValue::NULL, &JsValue::from_str(&json))
    {
      tracing::warn!(?error, "the page did not take the call state");
    }
  }
}

/// Owns the call: applies controls and turns engine events into the call view.
async fn run_media(
  mut call: WebCall,
  mut controls: mpsc::UnboundedReceiver<Control>,
  host: Weak<Host>,
  own_device: DeviceId,
) {
  let mut roster = CallRoster::new(own_device);
  loop {
    let next = {
      let event = pin!(call.next_event());
      let control = pin!(controls.next());
      match select(event, control).await {
        Either::Left((event, _)) => Either::Left(event),
        Either::Right((control, _)) => Either::Right(control),
      }
    };
    // The page signed out and dropped the host: leave.
    let Some(host) = host.upgrade() else {
      break;
    };
    match next {
      Either::Left(Some(event)) => host.on_engine_event(&mut roster, event),
      // The call ended on its own (the SFU closed it, or rejoining failed).
      Either::Left(None) => break,
      Either::Right(None | Some(Control::Leave)) => break,
      Either::Right(Some(control)) => apply(&call, control).await,
    }
  }
  let result = call.leave().await.map(|stats| {
    tracing::info!(
      frames_sent = stats.frames_sent,
      frames_received = stats.frames_received,
      "left the call"
    );
  });
  if let Some(host) = host.upgrade() {
    host.finish(result);
  }
}

async fn apply(call: &WebCall, control: Control) {
  match control {
    Control::Mute(is_muted) => call.set_muted(is_muted),
    Control::Deafen(is_deafened) => call.set_deafened(is_deafened),
    // The asker may have given up waiting.
    Control::Audio { source, sink, done } => drop(done.send(call.set_audio(source, sink).await)),
    Control::Camera { stream, done } => drop(done.send(call.share_camera_stream(stream).await)),
    Control::StopCamera => call.stop_camera(),
    Control::Screen { done } => drop(done.send(call.start_screen_share().await)),
    Control::StopScreen => call.stop_screen_share(),
    Control::Watch { stream_id, canvas } => call.watch_video(stream_id, canvas),
    Control::Unwatch(stream_id) => call.unwatch_video(stream_id),
    Control::StatusSounds(sounds) => call.set_status_cues(status_cues(sounds)),
    Control::Leave => {},
  }
}

impl CallKeyAuthenticator for PageAuthenticator {
  fn sign(&self, call_id: CallId, dave_public_key: &[u8]) -> Result<Vec<u8>, CallKeyError> {
    let host = host().map_err(|_| remote_error("the page is gone"))?;
    let answer = host
      .callbacks
      .sign
      .call2(
        &JsValue::NULL,
        &json_value(&call_id)?,
        &Uint8Array::from(dave_public_key),
      )
      .map_err(|error| remote_error(&js_text(&error)))?;
    bytes(answer)
  }

  fn verify(&self, call_id: CallId, device_id: DeviceId, binding: &[u8]) -> Result<Vec<u8>, CallKeyError> {
    let host = host().map_err(|_| remote_error("the page is gone"))?;
    let answer = host
      .callbacks
      .verify
      .call3(
        &JsValue::NULL,
        &json_value(&call_id)?,
        &json_value(&device_id)?,
        &Uint8Array::from(binding),
      )
      .map_err(|error| remote_error(&js_text(&error)))?;
    bytes(answer)
  }
}

impl WebJoinTicketSource for PageTickets {
  fn fetch(&self) -> LocalTicketFuture {
    Box::pin(async {
      let ticket_error = |reason: String| JoinTicketError { reason };
      let host = host().map_err(|_| ticket_error("the page is gone".into()))?;
      let promise: Promise = host
        .callbacks
        .ticket
        .call0(&JsValue::NULL)
        .and_then(JsCast::dyn_into)
        .map_err(|error| ticket_error(js_text(&error)))?;
      let answer = JsFuture::from(promise)
        .await
        .map_err(|error| ticket_error(js_text(&error)))?;
      let text = answer
        .as_string()
        .ok_or_else(|| ticket_error("the page answered no join token".into()))?;
      let response: CallJoinTokenResponse =
        serde_json::from_str(&text).map_err(|error| ticket_error(error.to_string()))?;
      Ok(JoinTicket {
        sfu_url: response.sfu_url,
        token: response.token,
      })
    })
  }
}

/// How a failed call action reads in the page: what to do about it, then the detail.
fn failure(error: WebCallError) -> ActionFailure {
  let kind = match &error {
    WebCallError::MediaAccess { input, reason, .. } => FailureKind::Call(media_failure(*input, *reason)),
    WebCallError::Unsupported(_) => FailureKind::Call(CallFailure::Unsupported),
    // The page's own failure text (a refused join token, offline), already in wire form.
    WebCallError::Ticket(error) => return ActionFailure::from_wire(&error.reason),
    WebCallError::Browser {
      context: "WebTransport connect",
      ..
    } => FailureKind::Network,
    _ => FailureKind::Other,
  };
  ActionFailure::new(kind, error.to_string())
}

fn media_failure(input: MediaInput, reason: MediaAccessReason) -> CallFailure {
  match (input, reason) {
    (MediaInput::Microphone, MediaAccessReason::Denied) => CallFailure::MicrophoneDenied,
    (MediaInput::Microphone, MediaAccessReason::NotFound) => CallFailure::MicrophoneMissing,
    (MediaInput::Microphone, _) => CallFailure::MicrophoneInUse,
    (MediaInput::Camera, MediaAccessReason::Denied) => CallFailure::CameraDenied,
    (MediaInput::Camera, MediaAccessReason::NotFound) => CallFailure::CameraMissing,
    (MediaInput::Camera, _) => CallFailure::CameraInUse,
    (MediaInput::Screen, _) => CallFailure::ScreenUnavailable,
  }
}

fn failure_value(failure: ActionFailure) -> JsValue {
  JsValue::from_str(&failure.to_wire())
}

fn host() -> Result<Rc<Host>, JsValue> {
  HOST
    .with(|slot| slot.borrow().clone())
    .ok_or_else(|| failure_value(ActionFailure::other("the call module was not started")))
}

fn function(object: &JsValue, name: &str) -> Result<Function, JsValue> {
  Reflect::get(object, &JsValue::from_str(name))?
    .dyn_into()
    .map_err(|_| JsValue::from_str(&format!("the page gave no `{name}` callback")))
}

fn media_devices(devices: &[AudioDeviceInfo]) -> Vec<MediaDevice> {
  devices
    .iter()
    // Before the page has permission, browsers list one anonymous device per kind.
    .filter(|device| !device.id.as_str().is_empty())
    .map(|device| MediaDevice {
      id: device.id.to_string(),
      label: device.name.clone(),
    })
    .collect()
}

fn device_choice(maybe_id: Option<String>) -> AudioDeviceChoice {
  match maybe_id {
    Some(id) if !id.is_empty() => AudioDeviceChoice::Device(AudioDeviceId::new(id)),
    _ => AudioDeviceChoice::Default,
  }
}

fn stop_tracks(stream: &MediaStream) {
  for track in stream.get_tracks().iter() {
    track.unchecked_into::<MediaStreamTrack>().stop();
  }
}

fn json_value<T: serde::Serialize>(value: &T) -> Result<JsValue, CallKeyError> {
  serde_json::to_string(value)
    .map(|text| JsValue::from_str(&text))
    .map_err(|error| remote_error(&error.to_string()))
}

fn bytes(answer: JsValue) -> Result<Vec<u8>, CallKeyError> {
  answer
    .dyn_into::<Uint8Array>()
    .map(|array| array.to_vec())
    .map_err(|_| remote_error("the page answered no bytes"))
}

fn remote_error(text: &str) -> CallKeyError {
  CallKeyError::Remote(text.to_owned())
}

fn js_text(error: &JsValue) -> String {
  error
    .as_string()
    .or_else(|| {
      Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
    })
    .unwrap_or_else(|| format!("{error:?}"))
}

/// Logs to the console like the app; the module has its own `tracing` dispatcher.
fn init_logging() {
  if IS_LOGGING.with(|is_logging| is_logging.replace(true)) {
    return;
  }
  console_error_panic_hook::set_once();
  let fmt_layer = tracing_subscriber::fmt::layer()
    .with_ansi(false)
    .without_time()
    .with_writer(tracing_web::MakeWebConsoleWriter::new());
  // Another dispatcher only exists if the page initialized this module twice; keep that one.
  let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
}
