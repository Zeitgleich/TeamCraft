//! The web client's calls (#330): a wasm module of its own that the page loads in the background
//! once the user is signed in, so the call engine doesn't add to the app's first download
//! (robustness.md, "Download size").
//!
//! The module keeps the call state the UI shows ([`CallView`]: rosters and ringing from the
//! gateway signals the page forwards, the active call from the media engine), plays the ringtone
//! and runs `teamcraft-media`'s browser driver ([`WebCall`]: WebTransport, WebCodecs, Web Audio,
//! DAVE). The page keeps the client core and the device key: it prepares joins, signs this
//! device's call key and verifies the others'. The contract between the two, and why it goes
//! through JavaScript, is in `teamcraft_client_api::page_calls`.
#![cfg(target_arch = "wasm32")]

mod ringtone;

use std::cell::{Cell, RefCell};
use std::pin::pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use futures::future::{Either, select};
use js_sys::{Function, Promise, Reflect, Uint8Array};
use teamcraft_client_api::call_view::{CallSignal, CallView};
use teamcraft_client_api::calls::{
  ActiveCall, BackgroundEffect, BrowserPermission, CallEndReason, CallRequest, CallStatusSounds,
  DevicePresence, JoinMedia, MediaDevice, MediaDevices, PushToTalk, ScreenShareSound,
};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_client_api::page_calls::PreparedCall;
use teamcraft_client_api::remote::{CallAction, CallActionAnswer, CallSnapshot};
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource};
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallKeyAuthenticator, CallKeyError};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_resilience::{
  JoinTicket, JoinTicketError, ResilienceConfig, certificate_hashes_from_hex,
};
use teamcraft_media::call_view::{CallRoster, status_cues};
use teamcraft_media::call_web::{
  self, LocalTicketFuture, MediaAccessReason, MediaInput, WebCall, WebCallConfig, WebCallError,
  WebJoinTicketSource,
};
use teamcraft_media::jitter::JitterBufferConfig;
use teamcraft_proto::{
  AudioCodec, CallId, DeviceId, MediaCapabilities, MediaStreamId, ScreenContentMode, VideoCodec,
};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{HtmlCanvasElement, HtmlVideoElement, MediaStream, MediaStreamTrack};

/// Simulcast layers the camera sends (as on desktop).
const MAX_SIMULCAST_LAYERS: u8 = 3;
/// Permission names for `navigator.permissions.query`.
const CAMERA_PERMISSION: &str = "camera";
const MICROPHONE_PERMISSION: &str = "microphone";

thread_local! {
  static HOST: RefCell<Option<Rc<Host>>> = const { RefCell::new(None) };
  static IS_LOGGING: Cell<bool> = const { Cell::new(false) };
}

/// The calls of this page and what the user chose for the next one.
struct Host {
  callbacks: Callbacks,
  state: RefCell<State>,
  /// The running call, if any.
  maybe_media: RefCell<Option<Media>>,
  /// Lists the devices again when the browser says they changed (kept for as long as the host):
  /// `devicechange`, and a camera or microphone permission that changed.
  _on_device_change: Closure<dyn FnMut()>,
  /// The `PermissionStatus` objects that call `_on_device_change`, kept so they stay alive.
  permission_statuses: RefCell<Vec<JsValue>>,
  /// Where the page serves the AudioWorklet module.
  worklet_url: String,
  /// The last state handed to the page: an unchanged one isn't handed again.
  last_sent: RefCell<String>,
}

/// The page's functions (see `page_calls`).
struct Callbacks {
  changed: Function,
  prepare: Function,
  sign: Function,
  verify: Function,
}

#[derive(Default)]
struct State {
  view: CallView,
  devices: MediaDevices,
  push_to_talk: PushToTalk,
  status_sounds: CallStatusSounds,
  microphone: AudioDeviceChoice,
  speakers: AudioDeviceChoice,
  maybe_camera: Option<String>,
  /// The workspace requires a background the browser can't apply (from the last preparation).
  requires_background: bool,
  /// The stream the camera sends, shown as the self view.
  maybe_local_camera: Option<MediaStream>,
  /// The self view: one `<video>` for the whole session, playing whichever camera is on.
  maybe_camera_view: Option<HtmlVideoElement>,
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
  MoveHere,
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
    mode: ScreenContentMode,
    done: Done,
  },
  StopScreen,
  Watch {
    stream_id: MediaStreamId,
    canvas: HtmlCanvasElement,
  },
  Unwatch(MediaStreamId),
  StatusSounds(CallStatusSounds),
  NetworkChanged,
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
  let worklet_url = Reflect::get(&callbacks, &JsValue::from_str("worklet"))?
    .as_string()
    .ok_or_else(|| JsValue::from_str("the page gave no `worklet` URL"))?;
  let callbacks = Callbacks {
    changed: function(&callbacks, "changed")?,
    prepare: function(&callbacks, "prepare")?,
    sign: function(&callbacks, "sign")?,
    verify: function(&callbacks, "verify")?,
  };
  let on_device_change = Closure::<dyn FnMut()>::new(|| {
    if let Ok(host) = host() {
      host.refresh_devices();
    }
  });
  watch_device_changes(&on_device_change);
  let host = Rc::new(Host {
    callbacks,
    state: RefCell::new(State::default()),
    maybe_media: RefCell::new(None),
    _on_device_change: on_device_change,
    permission_statuses: RefCell::new(Vec::new()),
    worklet_url,
    last_sent: RefCell::new(String::new()),
  });
  HOST.with(|slot| *slot.borrow_mut() = Some(host.clone()));
  spawn_local(watch_permissions(Rc::downgrade(&host)));
  host.refresh_devices();
  host.changed();
  Ok(())
}

/// Carries out a `CallAction` (JSON); resolves with nothing, or with the failure of a part of it
/// that didn't go through (`CallActionAnswer`).
#[wasm_bindgen]
pub async fn act(action: String) -> Result<JsValue, JsValue> {
  let host = host()?;
  let action: CallAction = serde_json::from_str(&action)
    .map_err(|error| failure_value(ActionFailure::other(format!("unreadable call action: {error}"))))?;
  let answer = host.act(action).await.map_err(failure_value)?;
  Ok(answer.map_or(JsValue::UNDEFINED, |partial| JsValue::from_str(&partial)))
}

/// A call event from the gateway (`CallSignal` JSON).
#[wasm_bindgen]
pub fn signal(signal: String) {
  match (host(), serde_json::from_str::<CallSignal>(&signal)) {
    (Ok(host), Ok(signal)) => {
      let is_changed = host.state.borrow_mut().view.apply_signal(signal);
      if is_changed {
        host.changed();
      }
    },
    (_, Err(error)) => tracing::warn!(%error, "unreadable call signal"),
    (Err(_), Ok(_)) => {},
  }
}

/// Starts or stops the ringtone of an incoming call.
#[wasm_bindgen]
pub fn ring(is_ringing: bool) {
  ringtone::set_ringing(is_ringing);
}

/// A new canvas that shows another device's track (`MediaStreamId` JSON) until unwatched.
#[wasm_bindgen]
pub fn watch_video(stream_id: String) -> Option<HtmlCanvasElement> {
  let (Ok(host), Ok(stream_id)) = (host(), serde_json::from_str(&stream_id)) else {
    return None;
  };
  let canvas: HtmlCanvasElement = web_sys::window()?
    .document()?
    .create_element("canvas")
    .ok()?
    .dyn_into()
    .ok()?;
  host.control(Control::Watch {
    stream_id,
    canvas: canvas.clone(),
  });
  Some(canvas)
}

#[wasm_bindgen]
pub fn unwatch_video(stream_id: String) {
  if let (Ok(host), Ok(stream_id)) = (host(), serde_json::from_str(&stream_id)) {
    host.control(Control::Unwatch(stream_id));
  }
}

/// The muted `<video>` playing the camera this device sends, for the self view. Always the same
/// element: a camera switched to another device plays in it, so the page never rebuilds it.
#[wasm_bindgen]
pub fn camera_view() -> Option<HtmlVideoElement> {
  let host = host().ok()?;
  let mut state = host.state.borrow_mut();
  if state.maybe_camera_view.is_none() {
    let video: HtmlVideoElement = web_sys::window()?
      .document()?
      .create_element("video")
      .ok()?
      .dyn_into()
      .ok()?;
    video.set_muted(true);
    video.set_autoplay(true);
    // iOS Safari plays inline only with this attribute.
    let _ = video.set_attribute("playsinline", "");
    state.maybe_camera_view = Some(video);
  }
  let video = state.maybe_camera_view.clone()?;
  video.set_src_object(state.maybe_local_camera.as_ref());
  Some(video)
}

/// The page's core saw the network come back or change, or the page woke up: a running call checks
/// its session, or retries a rejoin at once.
#[wasm_bindgen]
pub fn network_changed() {
  if let Ok(host) = host() {
    host.control(Control::NetworkChanged);
  }
}

/// Leaves a running call and forgets the page's callbacks (the page signs out).
#[wasm_bindgen]
pub async fn stop() {
  ringtone::set_ringing(false);
  let maybe_host = HOST.with(|slot| slot.borrow_mut().take());
  if let Some(host) = maybe_host {
    host.leave().await;
  }
}

impl Host {
  /// Joins the conversation's call with what the page's core prepared, leaving a running call, and
  /// starts the camera with it when chosen. A camera that can't start leaves the call running; the
  /// answer says why.
  async fn join(
    self: &Rc<Self>,
    request: CallRequest,
    media: JoinMedia,
  ) -> Result<CallActionAnswer, ActionFailure> {
    self.connect(request, media.is_muted).await?;
    if !media.is_camera_on {
      return Ok(None);
    }
    Ok(self.start_camera().await.err().map(|failure| failure.to_wire()))
  }

  /// Connects to the conversation's call, leaving a running call first.
  async fn connect(self: &Rc<Self>, request: CallRequest, is_muted: bool) -> Result<(), ActionFailure> {
    self.leave().await;
    let prepared = self.prepare(request.conversation).await?;
    let (microphone, speakers, sounds) = {
      let state = self.state.borrow();
      (
        state.microphone.clone(),
        state.speakers.clone(),
        state.status_sounds,
      )
    };
    let own_device = prepared.device_id;
    // Push-to-talk: the microphone opens only while the key is held.
    let is_muted_at_start = is_muted || self.state.borrow().push_to_talk.is_enabled;
    let config = WebCallConfig {
      sfu_url: prepared.sfu_url,
      certificate_hashes: certificate_hashes_from_hex(&prepared.certificate_sha256),
      token: prepared.token,
      engine: CallEngineConfig {
        user_id: prepared.user_id,
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
      maybe_trusted_roster: Some(prepared.trusted_devices),
      maybe_authenticator: Some(Arc::new(PageAuthenticator)),
      jitter: JitterBufferConfig::default(),
      resilience: ResilienceConfig::browser(),
      maybe_ticket_source: Some(Rc::new(PageTickets)),
      status_cues: status_cues(sounds),
      maybe_worklet_url: Some(self.worklet_url.clone()),
      is_muted_at_start,
    };
    let call = WebCall::join(config).await.map_err(failure)?;
    {
      let view = &mut self.state.borrow_mut().view;
      view.start_active(request, BackgroundEffect::None);
      if let Some(active) = view.maybe_active.as_mut() {
        active.is_muted = is_muted_at_start;
      }
    }
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
    // With the microphone allowed, the browser names its devices.
    self.refresh_devices();
    Ok(())
  }

  /// A join token and the trusted devices from the page's client core.
  async fn prepare(
    &self,
    conversation: teamcraft_proto::ConversationId,
  ) -> Result<PreparedCall, ActionFailure> {
    let id = serde_json::to_string(&conversation).map_err(|error| ActionFailure::other(error.to_string()))?;
    let answer = call_promise(&self.callbacks.prepare, &JsValue::from_str(&id)).await?;
    let text = answer
      .as_string()
      .ok_or_else(|| ActionFailure::other("the page prepared no call"))?;
    serde_json::from_str(&text).map_err(|error| ActionFailure::other(error.to_string()))
  }

  /// Lists the browser's devices again (they come and go, and get names with permission).
  fn refresh_devices(self: &Rc<Self>) {
    let host = Rc::downgrade(self);
    spawn_local(async move {
      let listed = match call_web::list_media_devices().await {
        Ok(listed) => listed,
        Err(error) => {
          tracing::debug!(%error, "could not list the media devices");
          return;
        },
      };
      let has_listed_any =
        !(listed.cameras.is_empty() && listed.inputs.is_empty() && listed.outputs.is_empty());
      let devices = MediaDevices {
        cameras: media_devices(&listed.cameras),
        microphones: media_devices(&listed.inputs),
        speakers: media_devices(&listed.outputs),
        camera_presence: presence(&listed.cameras, CAMERA_PERMISSION, has_listed_any).await,
        microphone_presence: presence(&listed.inputs, MICROPHONE_PERMISSION, has_listed_any).await,
      };
      if let Some(host) = host.upgrade() {
        host.state.borrow_mut().devices = devices;
        host.changed();
      }
    });
  }

  async fn act(self: &Rc<Self>, action: CallAction) -> Result<CallActionAnswer, ActionFailure> {
    match action {
      CallAction::Leave => self.leave().await,
      CallAction::SetMuted { is_muted } => self.control(Control::Mute(is_muted)),
      CallAction::SetDeafened { is_deafened } => self.control(Control::Deafen(is_deafened)),
      CallAction::MoveHere => self.control(Control::MoveHere),
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
        // An empty id is the "system default" entry.
        self.state.borrow_mut().maybe_camera = maybe_device.filter(|id| !id.is_empty());
        if self.state.borrow().view.is_camera_on() {
          self.start_camera().await?;
        }
      },
      CallAction::StartScreenShare {
        has_sound,
        content_mode,
      } => self.start_screen_share(has_sound, content_mode).await?,
      CallAction::StopScreenShare => {
        self.control(Control::StopScreen);
        self.update_active(|active| {
          active.is_sharing_screen = false;
          active.screen_sound = ScreenShareSound::Off;
        });
      },
      CallAction::SetStatusSounds { sounds } => {
        self.state.borrow_mut().status_sounds = sounds;
        self.control(Control::StatusSounds(sounds));
      },
      CallAction::Decline { conversation } => {
        self.state.borrow_mut().view.decline(conversation);
        self.changed();
      },
      CallAction::SetPushToTalk {
        is_enabled,
        maybe_shortcut,
      } => {
        {
          let mut state = self.state.borrow_mut();
          let shortcut = maybe_shortcut.unwrap_or_else(|| state.push_to_talk.shortcut.clone());
          // Browsers have no global shortcuts: the key works while the page has focus.
          state.push_to_talk = PushToTalk {
            is_enabled,
            shortcut,
            is_global: false,
            is_talking: false,
          };
        }
        self.control(Control::Mute(is_enabled));
        self.changed();
      },
      CallAction::SetTalking { is_talking } => {
        {
          let mut state = self.state.borrow_mut();
          if !state.push_to_talk.is_enabled || state.push_to_talk.is_talking == is_talking {
            return Ok(None);
          }
          state.push_to_talk.is_talking = is_talking;
        }
        self.control(Control::Mute(!is_talking));
        self.changed();
      },
      // The browser has no background effects (the UI offers none) and no system effects panel.
      CallAction::SetBackgroundEffect { .. } | CallAction::OpenSystemVideoEffects => {},
      CallAction::Join {
        request,
        is_muted,
        is_camera_on,
      } => {
        return self
          .join(
            request,
            JoinMedia {
              is_muted,
              is_camera_on,
            },
          )
          .await;
      },
      CallAction::RefreshDevices => self.refresh_devices(),
      // The page plays the test itself: it has to start inside the click (teamcraft-ui speaker_test).
      CallAction::TestSpeakers { .. } => {
        return Err(ActionFailure::other("the page plays the speaker test itself"));
      },
      CallAction::Unknown => return Err(ActionFailure::other("unknown call action")),
    }
    Ok(None)
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
    // Not in a call (its join failed and said why): nothing to send the camera to.
    if self.maybe_media.borrow().is_none() {
      return Ok(());
    }
    // admin.md §3.5: the browser can't put a background behind the camera image, so where the
    // organization requires one, the camera stays off.
    if self.state.borrow().requires_background {
      return Err(ActionFailure::new(
        FailureKind::Call(CallFailure::BackgroundRequired),
        "the organization requires a background, which the browser can't apply",
      ));
    }
    let maybe_camera = self.state.borrow().maybe_camera.clone();
    // Phones open one camera at a time: release the running one before asking for the other (the
    // flip button), or the browser refuses the second.
    let maybe_running = self.state.borrow_mut().maybe_local_camera.take();
    if let Some(running) = &maybe_running {
      stop_tracks(running);
    }
    let stream = match call_web::capture_camera(maybe_camera.as_deref()).await {
      Ok(stream) => stream,
      Err(error) => {
        if maybe_running.is_some() {
          // The old camera is gone, so don't claim it still sends.
          self.stop_camera();
        }
        return Err(failure(error));
      },
    };
    let sent = stream.clone();
    if let Err(error) = self.ask(|done| Control::Camera { stream: sent, done }).await {
      stop_tracks(&stream);
      return Err(error);
    }
    {
      let mut state = self.state.borrow_mut();
      if let Some(view) = &state.maybe_camera_view {
        view.set_src_object(Some(&stream));
      }
      state.maybe_local_camera = Some(stream);
    }
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
  async fn start_screen_share(&self, has_sound: bool, mode: ScreenContentMode) -> Result<(), ActionFailure> {
    match self.ask_media(|done| Control::Screen { mode, done }).await {
      Ok(()) => {},
      // The user closed the picker. A refusal by the system or a policy ("Permission denied by
      // system" in Chromium) is reported.
      Err(WebCallError::MediaAccess {
        input: MediaInput::Screen,
        reason: MediaAccessReason::Denied,
        message,
      }) if !message.contains("system") => return Ok(()),
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

  fn update_active(&self, change: impl FnOnce(&mut ActiveCall)) {
    if let Some(active) = self.state.borrow_mut().view.maybe_active.as_mut() {
      change(active);
    }
    self.changed();
  }

  fn changed(&self) {
    let snapshot = {
      let state = self.state.borrow();
      CallSnapshot {
        maybe_active: state.view.maybe_active.clone(),
        incoming: state.view.incoming.values().copied().collect(),
        running: state.view.running.values().cloned().collect(),
        devices: state.devices.clone(),
        video_tracks: state.view.video_tracks.values().cloned().collect(),
        push_to_talk: state.push_to_talk.clone(),
        maybe_end_reason: state.view.maybe_end_reason,
      }
    };
    let Ok(json) = serde_json::to_string(&snapshot) else {
      return;
    };
    if *self.last_sent.borrow() == json {
      return;
    }
    self.last_sent.replace(json.clone());
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
    Control::MoveHere => call.move_here(),
    // The asker may have given up waiting.
    Control::Audio { source, sink, done } => drop(done.send(call.set_audio(source, sink).await)),
    Control::Camera { stream, done } => drop(done.send(call.share_camera_stream(stream).await)),
    Control::StopCamera => call.stop_camera(),
    Control::Screen { mode, done } => drop(done.send(call.start_screen_share(mode).await)),
    Control::StopScreen => call.stop_screen_share(),
    Control::Watch { stream_id, canvas } => call.watch_video(stream_id, canvas),
    Control::Unwatch(stream_id) => call.unwatch_video(stream_id),
    Control::StatusSounds(sounds) => call.set_status_cues(status_cues(sounds)),
    Control::NetworkChanged => call.network_changed(),
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
      let conversation = host
        .state
        .borrow()
        .view
        .maybe_active
        .as_ref()
        .map(|active| active.conversation)
        .ok_or_else(|| ticket_error("no call to rejoin".into()))?;
      let prepared = host
        .prepare(conversation)
        .await
        .map_err(|failure| ticket_error(failure.to_wire()))?;
      // The SFU may have come back with another certificate (a restart): this token's hashes.
      Ok(JoinTicket {
        certificate_hashes: certificate_hashes_from_hex(&prepared.certificate_sha256),
        sfu_url: prepared.sfu_url,
        token: prepared.token,
      })
    })
  }
}

/// How a failed call action reads in the page: what to do about it, then the detail.
fn failure(error: WebCallError) -> ActionFailure {
  let kind = match &error {
    WebCallError::MediaAccess { input, reason, .. } => {
      media_failure(*input, *reason).map_or(FailureKind::Other, FailureKind::Call)
    },
    WebCallError::Unsupported(_) => FailureKind::Call(CallFailure::Unsupported),
    // The page's own failure text (a refused join token, offline), already in wire form.
    WebCallError::Ticket(error) => return ActionFailure::from_wire(&error.reason),
    // The chat server answered (it handed out the token); the SFU didn't: likely UDP blocked.
    WebCallError::Browser {
      context: "WebTransport connect",
      ..
    } => FailureKind::Call(CallFailure::ServerUnreachable),
    _ => FailureKind::Other,
  };
  ActionFailure::new(kind, error.to_string())
}

/// The call failure a refused device means; `None` when the browser gave no known reason.
fn media_failure(input: MediaInput, reason: MediaAccessReason) -> Option<CallFailure> {
  Some(match (input, reason) {
    (_, MediaAccessReason::Other) => return None,
    (MediaInput::Microphone, MediaAccessReason::Denied) => CallFailure::MicrophoneDenied,
    (MediaInput::Microphone, MediaAccessReason::NotFound) => CallFailure::MicrophoneMissing,
    (MediaInput::Microphone, MediaAccessReason::InUse) => CallFailure::MicrophoneInUse,
    (MediaInput::Camera, MediaAccessReason::Denied) => CallFailure::CameraDenied,
    (MediaInput::Camera, MediaAccessReason::NotFound) => CallFailure::CameraMissing,
    (MediaInput::Camera, MediaAccessReason::InUse) => CallFailure::CameraInUse,
    (MediaInput::Screen, _) => CallFailure::ScreenUnavailable,
  })
}

fn failure_value(failure: ActionFailure) -> JsValue {
  JsValue::from_str(&failure.to_wire())
}

fn host() -> Result<Rc<Host>, JsValue> {
  HOST
    .with(|slot| slot.borrow().clone())
    .ok_or_else(|| failure_value(ActionFailure::other("the call module was not started")))
}

/// Calls one of the page's async callbacks; a rejection is `ActionFailure` wire text.
async fn call_promise(function: &Function, argument: &JsValue) -> Result<JsValue, ActionFailure> {
  let promise: Promise = function
    .call1(&JsValue::NULL, argument)
    .and_then(JsCast::dyn_into)
    .map_err(|error| ActionFailure::other(js_text(&error)))?;
  JsFuture::from(promise)
    .await
    .map_err(|error| ActionFailure::from_wire(&js_text(&error)))
}

/// `navigator.mediaDevices.ondevicechange`: devices plugged in or out.
fn watch_device_changes(callback: &Closure<dyn FnMut()>) {
  let maybe_devices = web_sys::window()
    .and_then(|window| Reflect::get(&window.navigator(), &JsValue::from_str("mediaDevices")).ok())
    .filter(|devices| devices.is_object());
  if let Some(devices) = maybe_devices {
    // Without the property (old browsers) the lists are read at joins only.
    let _ = Reflect::set(&devices, &JsValue::from_str("ondevicechange"), callback.as_ref());
  }
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

/// What the listing says about one kind of input (`DevicePresence::in_browser`). Browsers list an
/// unnamed device both before the user was asked and after they refused; the permission tells
/// the two apart.
async fn presence(devices: &[AudioDeviceInfo], permission: &str, has_listed_any: bool) -> DevicePresence {
  let state = match permission_status(permission).await {
    Some(status) => Reflect::get(&status, &JsValue::from_str("state"))
      .ok()
      .and_then(|state| state.as_string())
      .map_or(BrowserPermission::Unknown, |state| {
        BrowserPermission::from_state(&state)
      }),
    None => BrowserPermission::Unknown,
  };
  DevicePresence::in_browser(
    devices.iter().map(|device| device.id.as_str()),
    state,
    has_listed_any,
  )
}

/// Lists the devices again when the user allows or blocks the camera or microphone (in the
/// address bar, say): browsers fire no `devicechange` for that.
async fn watch_permissions(host: Weak<Host>) {
  for name in [CAMERA_PERMISSION, MICROPHONE_PERMISSION] {
    let Some(status) = permission_status(name).await else {
      continue;
    };
    let Some(host) = host.upgrade() else {
      return;
    };
    // A browser whose status has no `onchange` just doesn't tell.
    let _ = Reflect::set(
      &status,
      &JsValue::from_str("onchange"),
      host._on_device_change.as_ref(),
    );
    host.permission_statuses.borrow_mut().push(status);
  }
}

/// `navigator.permissions.query({ name })`: the `PermissionStatus`; `None` where the browser
/// can't say (not every browser knows the camera and microphone permissions).
async fn permission_status(name: &str) -> Option<JsValue> {
  let navigator = web_sys::window()?.navigator();
  let permissions = Reflect::get(&navigator, &JsValue::from_str("permissions"))
    .ok()
    .filter(JsValue::is_object)?;
  let query: Function = Reflect::get(&permissions, &JsValue::from_str("query"))
    .ok()?
    .dyn_into()
    .ok()?;
  let descriptor = js_sys::Object::new();
  Reflect::set(&descriptor, &JsValue::from_str("name"), &JsValue::from_str(name)).ok()?;
  let promise: Promise = query.call1(&permissions, &descriptor).ok()?.dyn_into().ok()?;
  JsFuture::from(promise).await.ok()
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
  // INFO at most: the DAVE library logs key material at TRACE (security.md: never log keys).
  let fmt_layer = tracing_subscriber::fmt::layer()
    .with_ansi(false)
    .without_time()
    .with_writer(tracing_web::MakeWebConsoleWriter::new())
    .with_filter(LevelFilter::INFO);
  // Another dispatcher only exists if the page initialized this module twice; keep that one.
  let _ = tracing_subscriber::registry().with(fmt_layer).try_init();
}
