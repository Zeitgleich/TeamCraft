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

mod effects;
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
  ActiveCall, AudioSystem, BackgroundEffect, BrowserPermission, CallEndReason, CallPreview, CallRequest,
  CallStatusSounds, CameraBackground, DevicePresence, EffectsOffer, JoinMedia, MediaDevice, MediaDevices,
  OrgBackgrounds, PreviewCamera, PushToTalk, ScreenShareSound, VideoQuality,
};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_client_api::page_calls::{
  BrowserGraphics, EffectsCompositing, PreparedCall, browser_effects_compositing,
};
use teamcraft_client_api::remote::{CallAction, CallActionAnswer, CallSnapshot};
use teamcraft_client_api::video_stats::VideoStatsReport;
use teamcraft_media::audio_io::{AudioDeviceChoice, AudioDeviceId, AudioDeviceInfo, AudioSink, AudioSource};
use teamcraft_media::audio_level::SharedLevel;
use teamcraft_media::bwe::BweConfig;
use teamcraft_media::call_auth::{CallKeyAuthenticator, CallKeyError};
use teamcraft_media::call_engine::{CallEngineConfig, CallEngineEvent};
use teamcraft_media::call_resilience::{
  JoinTicket, JoinTicketError, ResilienceConfig, certificate_hashes_from_hex,
};
use teamcraft_media::call_view::{CallRoster, started_screen_sound, status_cues};
use teamcraft_media::call_web::{
  self, CameraEffects, LocalTicketFuture, LocalTrustFuture, MediaAccessReason, MediaInput, MicrophoneMeter,
  WebCall, WebCallConfig, WebCallError, WebJoinTicketSource, WebTrustSource, decode_picture,
};
use teamcraft_media::effects::{BackgroundEffect as MediaEffect, needs_picture};
use teamcraft_media::jitter::JitterBufferConfig;
use teamcraft_media::video::{ScreenProfile, VideoResolution, camera_limit};
use teamcraft_proto::{
  AudioCodec, CallId, CallRosterEntry, DeviceId, MediaCapabilities, MediaStreamId, ScreenContentMode,
  ScreenShareResolution, VideoTrackKind,
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
  /// The segmentation model and the last background, for the session (#439).
  effects: effects::Effects,
}

/// The page's functions (see `page_calls`).
struct Callbacks {
  changed: Function,
  prepare: Function,
  sign: Function,
  verify: Function,
  /// Shows a failure that no action waits for (`ActionFailure` wire text). A page from before it
  /// existed gives none: the failure is only logged then.
  maybe_failed: Option<Function>,
  /// The organization's background rule and the pictures (`page_calls`, #439). A page from before
  /// them gives none: no background effects then.
  maybe_background_rule: Option<Function>,
  maybe_background_picture: Option<Function>,
  /// The trusted devices afresh, for a device added to the conversation during the call
  /// (`page_calls`). A page from before it gives none: such a device ends the call then.
  maybe_roster: Option<Function>,
}

#[derive(Default)]
struct State {
  view: CallView,
  devices: MediaDevices,
  push_to_talk: PushToTalk,
  status_sounds: CallStatusSounds,
  /// The highest quality the camera is sent at (#967).
  camera_quality: VideoQuality,
  microphone: AudioDeviceChoice,
  speakers: AudioDeviceChoice,
  maybe_camera: Option<String>,
  /// The workspace requires an organization background (from the last preparation): what a page
  /// without the background callbacks tells.
  requires_background: bool,
  /// The background the user chose; the organization's rule may put another one behind the camera.
  background: BackgroundEffect,
  /// Whether the camera the call sends goes through the effects (it was started with an effect).
  is_camera_through_effects: bool,
  /// What the effects send, for the self view (a canvas' stream); the camera itself without them.
  maybe_effects_view: Option<MediaStream>,
  /// The camera the self view shows: the one the call sends, or the pre-join check's.
  maybe_local_camera: Option<MediaStream>,
  /// The self view: one `<video>` for the whole session, playing whichever camera is on.
  maybe_camera_view: Option<HtmlVideoElement>,
  /// What the pre-join screen asked to check (#442); an input starts when asked for, not again
  /// with every request, so a refused one isn't asked for over and over.
  preview: CallPreview,
  preview_camera: PreviewCamera,
  /// The pre-join check's microphone, for the meter.
  maybe_preview_meter: Option<MicrophoneMeter>,
}

/// The task that owns the [`WebCall`].
struct Media {
  controls: mpsc::UnboundedSender<Control>,
  finished: oneshot::Receiver<()>,
  /// The call's microphone level, for the device menu (#986).
  microphone_level: SharedLevel,
}

type Done = oneshot::Sender<Result<(), WebCallError>>;
/// Answers a screen share: whether its sound is sent.
type ScreenDone = oneshot::Sender<Result<bool, WebCallError>>;

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
    // Boxed: the effects pipeline dwarfs the other controls.
    maybe_effects: Option<Box<CameraEffects>>,
    /// What it is sent within (`video::camera_limit`).
    limit: VideoResolution,
    done: Done,
  },
  /// Another effect for a camera that goes through the effects.
  SetEffect(MediaEffect),
  StopCamera,
  Screen {
    profile: ScreenProfile,
    has_sound: bool,
    done: ScreenDone,
  },
  /// A screen the user already chose in a picker the page opened in the click (issue #930).
  PickedScreen {
    stream: MediaStream,
    profile: ScreenProfile,
    done: ScreenDone,
  },
  StopScreen,
  Watch {
    stream_id: MediaStreamId,
    canvas: HtmlCanvasElement,
  },
  Unwatch(MediaStreamId),
  /// The lines the viewer picked for a received track (`None`: Auto, #967).
  VideoQuality {
    stream_id: MediaStreamId,
    maybe_max_lines: Option<u16>,
  },
  StatusSounds(CallStatusSounds),
  NetworkChanged,
  VideoStats(oneshot::Sender<Option<VideoStatsReport>>),
  Leave,
  /// Leaves with the hang-up tone: a 1:1 call that ended for both (#997).
  HangUp,
}

/// Signs and verifies call key bindings in the page, which holds the device key (e2ee.md §8).
#[derive(Debug)]
struct PageAuthenticator;

/// Fresh join tokens from the page's client core, for rejoining after a lost connection.
struct PageTickets;

/// The call's trusted devices afresh from the page's client core (`roster`).
struct PageRoster {
  conversation: teamcraft_proto::ConversationId,
}

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
    maybe_failed: function(&callbacks, "failed").ok(),
    maybe_background_rule: function(&callbacks, "background_rule").ok(),
    maybe_background_picture: function(&callbacks, "background_picture").ok(),
    maybe_roster: function(&callbacks, "roster").ok(),
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
    effects: effects::Effects::default(),
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

/// What the call's video does now (stats for nerds, #938): `VideoStatsReport` JSON, or nothing
/// outside a call.
#[wasm_bindgen]
pub async fn video_stats() -> Option<String> {
  let host = host().ok()?;
  let (done, done_rx) = oneshot::channel();
  host.control(Control::VideoStats(done));
  let report = done_rx.await.ok()??;
  serde_json::to_string(&report).ok()
}

/// Opens the browser's screen picker at once (`ScreenContentMode` JSON) and returns the promise of
/// the chosen screen, for [`share_screen`]. The page calls it synchronously in the click handler:
/// Safari opens the picker only while the click's user activation lasts (issue #930). With
/// `has_sound` (left out by pages from before #441: no sound) the picker also offers the tab's or
/// the system's sound. `resolution` (`ScreenShareResolution` JSON, #565; left out: standard) is
/// the share's largest size. Throws when the browser can't open one.
#[wasm_bindgen]
pub fn pick_screen(
  content_mode: String,
  has_sound: Option<bool>,
  resolution: Option<String>,
) -> Result<Promise, JsValue> {
  let profile = screen_profile(&content_mode, resolution.as_deref())?;
  call_web::request_screen(&profile, has_sound.unwrap_or(false))
    .map_err(|error| failure_value(failure(error)))
}

/// A screen share's profile from the page's JSON: its mode and, when given, its size.
fn screen_profile(content_mode: &str, maybe_resolution: Option<&str>) -> Result<ScreenProfile, JsValue> {
  let unreadable = |what: &str, error: serde_json::Error| {
    failure_value(ActionFailure::other(format!("unreadable screen {what}: {error}")))
  };
  let mode: ScreenContentMode =
    serde_json::from_str(content_mode).map_err(|error| unreadable("mode", error))?;
  let size: ScreenShareResolution = match maybe_resolution {
    Some(json) => serde_json::from_str(json).map_err(|error| unreadable("resolution", error))?,
    None => ScreenShareResolution::default(),
  };
  Ok(ScreenProfile { mode, size })
}

/// Shares the screen chosen in a picker [`pick_screen`] opened (`StartScreenShare` with the
/// screen already asked for); resolves and rejects like [`act`].
#[wasm_bindgen]
pub async fn share_screen(
  has_sound: bool,
  content_mode: String,
  picked: Promise,
  resolution: Option<String>,
) -> Result<JsValue, JsValue> {
  let host = host()?;
  let profile = screen_profile(&content_mode, resolution.as_deref())?;
  host
    .start_screen_share(has_sound, profile, Some(picked))
    .await
    .map_err(failure_value)?;
  Ok(JsValue::UNDEFINED)
}

/// A call event from the gateway (`CallSignal` JSON). A 1:1 call this page is in that ended for
/// both is left with the hang-up tone (#997).
#[wasm_bindgen]
pub fn signal(signal: String) {
  match (host(), serde_json::from_str::<CallSignal>(&signal)) {
    (Ok(host), Ok(signal)) => {
      let (is_end_of_active, is_changed) = {
        let view = &mut host.state.borrow_mut().view;
        (view.is_end_of_active(&signal), view.apply_signal(signal))
      };
      if is_changed {
        host.changed();
      }
      if is_end_of_active {
        host.control(Control::HangUp);
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
  video.set_src_object(state.shown_camera());
  Some(video)
}

/// The microphone's level, from 0 (silence, or nothing measured) to 1: the pre-join check's, or
/// the running call's for its device menu (#986).
#[wasm_bindgen]
pub fn preview_level() -> f32 {
  let Ok(host) = host() else {
    return 0.0;
  };
  let maybe_check = host
    .state
    .borrow()
    .maybe_preview_meter
    .as_ref()
    .map(MicrophoneMeter::level);
  maybe_check
    .or_else(|| {
      host
        .maybe_media
        .borrow()
        .as_ref()
        .map(|media| media.microphone_level.get())
    })
    .unwrap_or(0.0)
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
    host.end_preview(false);
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
    // The call opens the microphone itself; the check's camera goes on as the call's (no second
    // prompt, no gap in the self view).
    let maybe_checked_camera = self.end_preview(media.is_camera_on);
    if let Err(failure) = self.connect(request, media.is_muted).await {
      if let Some(stream) = &maybe_checked_camera {
        stop_tracks(stream);
        if let Some(view) = &self.state.borrow().maybe_camera_view {
          view.set_src_object(None);
        }
      }
      return Err(failure);
    }
    if !media.is_camera_on {
      return Ok(None);
    }
    let started = self.start_camera_from(maybe_checked_camera).await;
    Ok(started.err().map(|failure| failure.to_wire()))
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
    let ends_when_one_side_leaves = prepared.ends_when_one_side_leaves;
    // AV1 for screen shares where this browser encodes and decodes it (#565).
    let codecs = call_web::video_codec_support().await;
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
          video_encoders: codecs.encoders,
          video_decoders: codecs.decoders,
          max_simulcast_layers: MAX_SIMULCAST_LAYERS,
          max_frame_fragments: teamcraft_proto::MAX_VIDEO_FRAME_FRAGMENTS,
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
      maybe_trust_source: self.callbacks.maybe_roster.is_some().then(|| {
        Rc::new(PageRoster {
          conversation: request.conversation,
        }) as Rc<dyn WebTrustSource>
      }),
      status_cues: status_cues(sounds),
      maybe_worklet_url: Some(self.worklet_url.clone()),
      is_muted_at_start,
    };
    let requires_background = prepared.requires_background;
    let call = WebCall::join(config).await.map_err(failure)?;
    {
      let state = &mut *self.state.borrow_mut();
      state.requires_background = requires_background;
      let view = &mut state.view;
      view.set_this_device(CallRosterEntry {
        user_id: prepared.user_id,
        device_id: own_device,
      });
      view.start_active(request, BackgroundEffect::None);
      view.set_ends_when_one_side_leaves(ends_when_one_side_leaves);
      if let Some(active) = view.maybe_active.as_mut() {
        active.is_muted = is_muted_at_start;
      }
    }
    let (controls, control_rx) = mpsc::unbounded();
    let (finished_tx, finished) = oneshot::channel();
    *self.maybe_media.borrow_mut() = Some(Media {
      controls,
      finished,
      microphone_level: call.microphone_level(),
    });
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
        // The browser's own; it doesn't say which.
        audio_system: AudioSystem::Unknown,
      };
      if let Some(host) = host.upgrade() {
        host.state.borrow_mut().devices = devices;
        host.changed();
      }
    });
  }

  async fn act(self: &Rc<Self>, action: CallAction) -> Result<CallActionAnswer, ActionFailure> {
    match action {
      // Leaving a 1:1 call that then ends for both plays the hang-up tone (#997).
      CallAction::Leave if self.state.borrow().view.is_leave_a_hang_up() => {
        self.end_media(Control::HangUp).await
      },
      CallAction::Leave => self.leave().await,
      CallAction::SetMuted { is_muted } => self.control(Control::Mute(is_muted)),
      CallAction::SetDeafened { is_deafened } => self.control(Control::Deafen(is_deafened)),
      CallAction::MoveHere => self.control(Control::MoveHere),
      CallAction::SelectMicrophone { maybe_device } => {
        self.state.borrow_mut().microphone = device_choice(maybe_device);
        self.switch_audio().await?;
        self.restart_preview_microphone().await?;
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
        self.restart_preview_camera().await?;
      },
      CallAction::StartScreenShare {
        has_sound,
        content_mode,
        resolution,
      } => {
        let profile = ScreenProfile {
          mode: content_mode,
          size: resolution,
        };
        self.start_screen_share(has_sound, profile, None).await?
      },
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
      CallAction::SetCameraQuality { quality } => {
        let previous = std::mem::replace(&mut self.state.borrow_mut().camera_quality, quality);
        if previous.max_lines() != quality.max_lines() {
          // A running camera (or the pre-join check's) opens again at the new size.
          if self.maybe_media.borrow().is_some() && self.state.borrow().view.is_camera_on() {
            self.start_camera().await?;
          }
          self.restart_preview_camera().await?;
        }
      },
      CallAction::SetVideoQuality { stream_id, quality } => self.control(Control::VideoQuality {
        stream_id,
        maybe_max_lines: quality.max_lines(),
      }),
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
      CallAction::SetBackgroundEffect { effect } => self.set_background(effect).await?,
      // The browser has no system effects panel.
      CallAction::OpenSystemVideoEffects => {},
      // The decoders are the desktop page's; this module asks its browser itself.
      CallAction::Join {
        request,
        is_muted,
        is_camera_on,
        video_decoders: _,
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
      CallAction::SetPreview { preview } => self.set_preview(preview).await?,
      // The page plays the test itself: it has to start inside the click (teamcraft-ui speaker_test).
      CallAction::TestSpeakers { .. } => {
        return Err(ActionFailure::other("the page plays the speaker test itself"));
      },
      CallAction::Unknown => return Err(ActionFailure::other("unknown call action")),
    }
    Ok(None)
  }

  async fn leave(&self) {
    self.end_media(Control::Leave).await;
  }

  /// Ends the running call with `control` (a leave or a hang-up) and waits until it finished.
  async fn end_media(&self, control: Control) {
    let maybe_media = self.maybe_media.borrow_mut().take();
    if let Some(media) = maybe_media {
      // A closed channel means the call already ended; waiting still lets it finish.
      let _ = media.controls.unbounded_send(control);
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
    self.start_camera_from(None).await
  }

  /// Sends the camera: `maybe_open`, one the pre-join check opened, or a newly opened one.
  async fn start_camera_from(&self, maybe_open: Option<MediaStream>) -> Result<(), ActionFailure> {
    // Not in a call (its join failed and said why): nothing to send the camera to.
    if self.maybe_media.borrow().is_none() {
      if let Some(stream) = &maybe_open {
        stop_tracks(stream);
      }
      return Ok(());
    }
    // admin.md §3.5: where the organization requires a background, the camera starts only with
    // one of its pictures behind the person (#439), or not at all.
    let (shown, maybe_effects) = match self.camera_effects().await {
      Ok(plan) => plan,
      Err(failure) => {
        if let Some(stream) = &maybe_open {
          stop_tracks(stream);
        }
        return Err(failure);
      },
    };
    let stream = match maybe_open {
      Some(stream) => stream,
      None => self.reopen_camera().await?,
    };
    // The self view shows what the effects send.
    let maybe_view = maybe_effects.as_ref().and_then(|_| effects::self_view());
    let maybe_effects = maybe_effects.map(|effects| match &maybe_view {
      Some((canvas, _)) => Box::new(effects.with_view(canvas.clone())),
      None => Box::new(effects),
    });
    let is_through_effects = maybe_effects.is_some();
    let sent = stream.clone();
    let limit = self.camera_limit();
    let started = self
      .ask(|done| Control::Camera {
        stream: sent,
        maybe_effects,
        limit,
        done,
      })
      .await;
    if let Err(error) = started {
      stop_tracks(&stream);
      return Err(error);
    }
    {
      let mut state = self.state.borrow_mut();
      if let Some(old) = state.maybe_effects_view.take() {
        stop_tracks(&old);
      }
      state.maybe_effects_view = maybe_view.map(|(_, view)| view);
      state.is_camera_through_effects = is_through_effects;
      state.maybe_local_camera = Some(stream);
      if let Some(view) = &state.maybe_camera_view {
        view.set_src_object(state.shown_camera());
      }
    }
    self.update_active(|active| {
      active.is_camera_on = true;
      active.background = shown;
    });
    Ok(())
  }

  /// The user chose another background: a camera through the effects changes it at once, a
  /// camera without them starts again through them when there is an effect to apply (#439).
  async fn set_background(&self, effect: BackgroundEffect) -> Result<(), ActionFailure> {
    self.state.borrow_mut().background = effect;
    let is_camera_on = self.maybe_media.borrow().is_some() && self.state.borrow().view.is_camera_on();
    if !is_camera_on {
      return Ok(());
    }
    let plan = match self.background_plan().await {
      Ok(plan) => plan,
      // A required background that can't be applied any more: the room must not show.
      Err(failure) => {
        self.stop_camera();
        return Err(failure);
      },
    };
    if self.state.borrow().is_camera_through_effects {
      // The camera keeps its pipeline; only the effect changes.
      self.control(Control::SetEffect(plan.maybe_effect.unwrap_or(MediaEffect::None)));
      self.update_active(|active| active.background = plan.shown);
      Ok(())
    } else if plan.maybe_effect.is_some() {
      self.start_camera().await
    } else {
      Ok(())
    }
  }

  /// The effects a starting camera goes through, and the background shown (for the call state).
  /// Effects that can't start leave the camera without them (and say so); where a background is
  /// required, the camera can't start then.
  async fn camera_effects(&self) -> Result<(BackgroundEffect, Option<CameraEffects>), ActionFailure> {
    let plan = self.background_plan().await?;
    let Some(effect) = plan.maybe_effect else {
      return Ok((plan.shown, None));
    };
    let Some(compositing) = self.effects_compositing() else {
      // background_plan only plans an effect where the browser runs them.
      self.effect_failed(plan.is_required, "this browser can't composite the effects")?;
      return Ok((BackgroundEffect::None, None));
    };
    match self.effects.camera_effects(compositing, effect).await {
      Ok(effects) => {
        let effects = if plan.is_required {
          effects.required()
        } else {
          effects
        };
        Ok((plan.shown, Some(effects)))
      },
      Err(error) => {
        self.effect_failed(plan.is_required, &error.to_string())?;
        Ok((BackgroundEffect::None, None))
      },
    }
  }

  /// What goes behind the camera under the organization's rule, as the pipeline's effect (an own
  /// or organization picture is fetched and decoded, an illustration drawn, the first time).
  async fn background_plan(&self) -> Result<BackgroundPlan, ActionFailure> {
    // The photos are only in the apps (a choice synced from one of them, say): none here.
    let requested = Some(self.state.borrow().background)
      .filter(|requested| EffectsOffer::InCall.offers(*requested))
      .unwrap_or_default();
    let rule = self.background_rule().await;
    let is_required = rule.is_required();
    let chosen = match rule.camera_background(requested, self.effects_compositing().is_some()) {
      CameraBackground::Plain => return Ok(BackgroundPlan::plain(is_required)),
      CameraBackground::Refused => {
        return Err(background_required("no background can be applied here"));
      },
      CameraBackground::Effect(chosen) => chosen,
    };
    match self.chosen_effect(chosen).await {
      Ok(effect) => Ok(BackgroundPlan {
        shown: chosen,
        maybe_effect: Some(effect),
        is_required,
      }),
      Err(error) => {
        self.effect_failed(is_required, &error)?;
        Ok(BackgroundPlan::plain(is_required))
      },
    }
  }

  /// `chosen` as the pipeline's effect, kept for the next time.
  async fn chosen_effect(&self, chosen: BackgroundEffect) -> Result<MediaEffect, String> {
    if let Some(effect) = self.effects.cached(chosen) {
      return Ok(effect);
    }
    let maybe_picture = if needs_picture(chosen) {
      let file = self.background_picture(chosen).await?;
      Some(decode_picture(&file).await.map_err(|error| error.to_string())?)
    } else {
      None
    };
    self
      .effects
      .effect(chosen, maybe_picture)
      .map_err(|error| error.to_string())
  }

  /// An effect that can't be applied: the camera goes on without it (the user hears why), unless
  /// the organization requires a background.
  fn effect_failed(&self, is_required: bool, error: &str) -> Result<(), ActionFailure> {
    if is_required {
      return Err(background_required(error));
    }
    self.report(&ActionFailure::new(
      FailureKind::Call(CallFailure::EffectsUnavailable),
      error,
    ));
    Ok(())
  }

  /// How this browser composites the effects, where it runs them and the page hands over what
  /// they need.
  fn effects_compositing(&self) -> Option<EffectsCompositing> {
    let has_callbacks =
      self.callbacks.maybe_background_rule.is_some() && self.callbacks.maybe_background_picture.is_some();
    if !has_callbacks {
      return None;
    }
    let window = web_sys::window()?;
    let (has_webgpu, has_webgl2, has_wasm_simd) = teamcraft_media::web_api::effects_graphics();
    let graphics = BrowserGraphics {
      has_webgpu,
      has_webgl2,
      has_wasm_simd,
    };
    browser_effects_compositing(graphics, &window.navigator().user_agent().unwrap_or_default())
  }

  /// The organization's background rule from the page's core. Without the callback (an older page)
  /// only whether one is required is known; a rule that can't be read restricts nothing beyond
  /// that, as on desktop.
  async fn background_rule(&self) -> OrgBackgrounds {
    let fallback = || required_only(self.state.borrow().requires_background);
    let Some(rule) = &self.callbacks.maybe_background_rule else {
      return fallback();
    };
    match call_promise(rule, &JsValue::UNDEFINED).await {
      Ok(answer) => answer
        .as_string()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_else(fallback),
      Err(failure) => {
        tracing::warn!(detail = %failure.detail, "the organization's background rule is unknown");
        fallback()
      },
    }
  }

  /// The file of an own or organization picture from the page's core.
  async fn background_picture(&self, effect: BackgroundEffect) -> Result<Vec<u8>, String> {
    let picture = self
      .callbacks
      .maybe_background_picture
      .as_ref()
      .ok_or("the page hands over no pictures")?;
    let json = serde_json::to_string(&effect).map_err(|error| error.to_string())?;
    let answer = call_promise(picture, &JsValue::from_str(&json))
      .await
      .map_err(|failure| failure.detail)?;
    answer
      .dyn_into::<Uint8Array>()
      .map(|bytes| bytes.to_vec())
      .map_err(|_| "the page handed over no picture".to_owned())
  }

  /// The box the camera is sent within, by the user's quality setting (#967).
  fn camera_limit(&self) -> VideoResolution {
    camera_limit(self.state.borrow().camera_quality.max_lines())
  }

  /// Opens the chosen camera again (the browser may ask) for the call.
  async fn reopen_camera(&self) -> Result<MediaStream, ActionFailure> {
    let maybe_camera = self.state.borrow().maybe_camera.clone();
    // Phones open one camera at a time: release the running one before asking for the other (the
    // flip button), or the browser refuses the second.
    let maybe_running = self.state.borrow_mut().maybe_local_camera.take();
    if let Some(running) = &maybe_running {
      stop_tracks(running);
    }
    call_web::capture_camera(maybe_camera.as_deref(), self.camera_limit())
      .await
      .map_err(|error| {
        if maybe_running.is_some() {
          // The old camera is gone, so don't claim it still sends.
          self.stop_camera();
        }
        failure(error)
      })
  }

  /// Runs the pre-join check as asked (#442). A call on this device has the camera and
  /// microphone; then there is nothing to check.
  async fn set_preview(&self, preview: CallPreview) -> Result<(), ActionFailure> {
    if self.maybe_media.borrow().is_some() {
      return Ok(());
    }
    let previous = std::mem::replace(&mut self.state.borrow_mut().preview, preview);
    // Both inputs, even when the first fails; the first failure is the answer.
    let camera = match (previous.is_camera_on, preview.is_camera_on) {
      (false, true) => self.start_preview_camera().await,
      (true, false) => {
        self.stop_preview_camera();
        Ok(())
      },
      _ => Ok(()),
    };
    let microphone = match (previous.is_mic_on, preview.is_mic_on) {
      (false, true) => self.start_preview_microphone().await,
      (true, false) => {
        self.state.borrow_mut().maybe_preview_meter = None;
        Ok(())
      },
      _ => Ok(()),
    };
    camera.and(microphone)
  }

  /// Opens the camera for the self view only (the browser may ask).
  async fn start_preview_camera(&self) -> Result<(), ActionFailure> {
    self.set_preview_camera(PreviewCamera::Starting);
    let maybe_camera = self.state.borrow().maybe_camera.clone();
    let result = call_web::capture_camera(maybe_camera.as_deref(), self.camera_limit()).await;
    // Ended (or restarted) meanwhile: the screen closed, or a join took over.
    if self.state.borrow().preview_camera != PreviewCamera::Starting {
      if let Ok(stream) = &result {
        stop_tracks(stream);
      }
      return Ok(());
    }
    match result {
      Ok(stream) => {
        {
          let mut state = self.state.borrow_mut();
          if let Some(view) = &state.maybe_camera_view {
            view.set_src_object(Some(&stream));
          }
          state.maybe_local_camera = Some(stream);
        }
        self.set_preview_camera(PreviewCamera::Live);
        Ok(())
      },
      Err(error) => {
        self.set_preview_camera(PreviewCamera::Failed);
        Err(failure(error))
      },
    }
  }

  fn stop_preview_camera(&self) {
    {
      let mut state = self.state.borrow_mut();
      if let Some(stream) = state.maybe_local_camera.take() {
        stop_tracks(&stream);
      }
      if let Some(view) = &state.maybe_camera_view {
        view.set_src_object(None);
      }
    }
    self.set_preview_camera(PreviewCamera::Off);
  }

  /// Another camera was chosen: the check shows it instead.
  async fn restart_preview_camera(&self) -> Result<(), ActionFailure> {
    let is_checking = self.maybe_media.borrow().is_none() && self.state.borrow().preview.is_camera_on;
    if !is_checking {
      return Ok(());
    }
    self.stop_preview_camera();
    self.start_preview_camera().await
  }

  async fn start_preview_microphone(&self) -> Result<(), ActionFailure> {
    let choice = self.state.borrow().microphone.clone();
    let meter = MicrophoneMeter::open(&choice).await.map_err(failure)?;
    // Not asked for any more (the screen closed, a join took over): the meter goes, and with it
    // the microphone.
    let is_wanted = self.maybe_media.borrow().is_none() && self.state.borrow().preview.is_mic_on;
    if is_wanted {
      self.state.borrow_mut().maybe_preview_meter = Some(meter);
    }
    Ok(())
  }

  /// Another microphone was chosen: the meter measures it instead.
  async fn restart_preview_microphone(&self) -> Result<(), ActionFailure> {
    let is_checking = self.maybe_media.borrow().is_none() && self.state.borrow().preview.is_mic_on;
    if !is_checking {
      return Ok(());
    }
    self.state.borrow_mut().maybe_preview_meter = None;
    self.start_preview_microphone().await
  }

  /// Ends the pre-join check; with `keeps_camera` its running camera is handed back (for the
  /// call) instead of stopped.
  fn end_preview(&self, keeps_camera: bool) -> Option<MediaStream> {
    let maybe_camera = {
      let mut state = self.state.borrow_mut();
      state.preview = CallPreview::default();
      state.maybe_preview_meter = None;
      let is_checking_camera = state.preview_camera != PreviewCamera::Off;
      // Only the check's camera: a call's goes with the call.
      if is_checking_camera && self.maybe_media.borrow().is_none() {
        state.maybe_local_camera.take()
      } else {
        None
      }
    };
    self.set_preview_camera(PreviewCamera::Off);
    match maybe_camera {
      Some(stream) if keeps_camera => Some(stream),
      Some(stream) => {
        stop_tracks(&stream);
        None
      },
      None => None,
    }
  }

  fn set_preview_camera(&self, camera: PreviewCamera) {
    self.state.borrow_mut().preview_camera = camera;
    self.changed();
  }

  fn stop_camera(&self) {
    self.control(Control::StopCamera);
    self.state.borrow_mut().release_camera();
    self.update_active(|active| active.is_camera_on = false);
  }

  /// Shares a screen or window: the one chosen in `maybe_picked`, the picker the page opened in the
  /// click (issue #930), or else asks for one now. Closing the browser's picker is not a failure.
  /// Asked-for sound goes along where the browser captured it (the tab's or the system's sound in
  /// Chromium, #441); where it didn't (Safari, Firefox, "share audio" unticked), the picture goes
  /// alone and the sound shows as unavailable.
  async fn start_screen_share(
    &self,
    has_sound: bool,
    profile: ScreenProfile,
    maybe_picked: Option<Promise>,
  ) -> Result<(), ActionFailure> {
    let result = match maybe_picked {
      Some(picked) => self.share_picked_screen(picked, profile, has_sound).await,
      None => {
        self
          .ask_screen(|done| Control::Screen {
            profile,
            has_sound,
            done,
          })
          .await
      },
    };
    let is_sound_sent = match result {
      Ok(Some(is_sound_sent)) => is_sound_sent,
      // The call ended meanwhile.
      Ok(None) => return Ok(()),
      Err(error) if error.is_screen_picker_closed() => return Ok(()),
      Err(error) => return Err(failure(error)),
    };
    self.update_active(|active| {
      active.is_sharing_screen = true;
      active.screen_sound = started_screen_sound(has_sound, is_sound_sent);
    });
    Ok(())
  }

  /// Sends a screen control to the call task and waits for whether the share's sound is sent;
  /// `None` without a call.
  async fn ask_screen(
    &self,
    control: impl FnOnce(ScreenDone) -> Control,
  ) -> Result<Option<bool>, WebCallError> {
    let (done, answer) = oneshot::channel();
    let is_sent = self
      .maybe_media
      .borrow()
      .as_ref()
      .is_some_and(|media| media.controls.unbounded_send(control(done)).is_ok());
    if !is_sent {
      return Ok(None);
    }
    // No answer: the call ended meanwhile.
    answer.await.map_or(Ok(None), |result| result.map(Some))
  }

  /// Waits for the user's choice in a picker the page opened and sends that screen. The stream is
  /// this module's from here: when the call ended meanwhile or the share fails, it stops, so the
  /// browser doesn't keep capturing (and showing that it does). Sound the user didn't ask for is
  /// not sent.
  async fn share_picked_screen(
    &self,
    picked: Promise,
    profile: ScreenProfile,
    has_sound: bool,
  ) -> Result<Option<bool>, WebCallError> {
    let stream = call_web::picked_screen(picked).await?;
    if !has_sound {
      for track in stream.get_audio_tracks().iter() {
        let track: MediaStreamTrack = track.unchecked_into();
        stream.remove_track(&track);
        track.stop();
      }
    }
    let (done, answer) = oneshot::channel();
    let sent = stream.clone();
    let is_sent = self.maybe_media.borrow().as_ref().is_some_and(|media| {
      media
        .controls
        .unbounded_send(Control::PickedScreen {
          stream: sent,
          profile,
          done,
        })
        .is_ok()
    });
    let result = if is_sent {
      // No answer: the call ended meanwhile.
      answer
        .await
        .map_err(|_| WebCallError::Ended)
        .and_then(|result| result)
    } else {
      Err(WebCallError::Ended)
    };
    match result {
      Ok(is_sound_sent) => Ok(Some(is_sound_sent)),
      Err(WebCallError::Ended) => {
        stop_tracks(&stream);
        Ok(None)
      },
      Err(error) => {
        stop_tracks(&stream);
        Err(error)
      },
    }
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
    if let CallEngineEvent::OwnVideoEnded {
      kind: VideoTrackKind::Camera,
      ..
    } = &event
    {
      self.on_camera_ended();
    }
    let is_changed = roster.apply(&mut self.state.borrow_mut().view, event);
    if is_changed {
      self.changed();
    }
  }

  /// The call stopped the camera, which ended by itself (unplugged, the permission revoked,
  /// another app took it, or its pictures could no longer be encoded; issue #920). The roster
  /// turns it off in the call state; its stream leaves the self view, and the user hears why the
  /// video is off. A camera started meanwhile replaces the stream only after this.
  fn on_camera_ended(&self) {
    {
      let mut state = self.state.borrow_mut();
      state.release_camera();
      if let Some(view) = &state.maybe_camera_view {
        view.set_src_object(None);
      }
    }
    self.report(&ActionFailure::new(
      FailureKind::Call(CallFailure::CameraStopped),
      "the camera ended by itself",
    ));
  }

  /// Shows a failure no action waits for.
  fn report(&self, failure: &ActionFailure) {
    tracing::warn!(detail = %failure.detail, "call failure");
    let Some(failed) = &self.callbacks.maybe_failed else {
      return;
    };
    if let Err(error) = failed.call1(&JsValue::NULL, &JsValue::from_str(&failure.to_wire())) {
      tracing::warn!(?error, "the page did not take the failure");
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
      state.release_camera();
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
        preview_camera: state.preview_camera,
        // The browser opens the camera the user picks or none: no fallback here.
        maybe_camera_fallback: None,
        maybe_this_device: state.view.maybe_this_device,
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
  let mut is_hang_up = false;
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
      Either::Right(Some(Control::HangUp)) => {
        is_hang_up = true;
        break;
      },
      Either::Right(Some(control)) => apply(&call, control).await,
    }
  }
  let left = if is_hang_up {
    call.hang_up().await
  } else {
    call.leave().await
  };
  let result = left.map(|stats| {
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
    Control::Camera {
      stream,
      maybe_effects,
      limit,
      done,
    } => {
      let result = match maybe_effects {
        Some(effects) => {
          call
            .share_camera_stream_with_effects(stream, *effects, limit)
            .await
        },
        None => call.share_camera_stream(stream, limit).await,
      };
      drop(done.send(result));
    },
    Control::SetEffect(effect) => call.set_background_effect(effect),
    Control::StopCamera => call.stop_camera(),
    Control::Screen {
      profile,
      has_sound,
      done,
    } => drop(done.send(call.start_screen_share(&profile, has_sound).await)),
    Control::PickedScreen {
      stream,
      profile,
      done,
    } => drop(done.send(call.share_screen_stream(stream, &profile).await)),
    Control::StopScreen => call.stop_screen_share(),
    Control::Watch { stream_id, canvas } => call.watch_video(stream_id, canvas),
    Control::Unwatch(stream_id) => call.unwatch_video(stream_id),
    Control::VideoQuality {
      stream_id,
      maybe_max_lines,
    } => call.set_video_quality(stream_id, maybe_max_lines),
    Control::StatusSounds(sounds) => call.set_status_cues(status_cues(sounds)),
    Control::NetworkChanged => call.network_changed(),
    Control::VideoStats(done) => drop(done.send(call.video_stats().await)),
    Control::Leave | Control::HangUp => {},
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

impl WebTrustSource for PageRoster {
  fn fetch(&self) -> LocalTrustFuture {
    let conversation = self.conversation;
    Box::pin(async move {
      let host = host().map_err(|_| "the page is gone".to_owned())?;
      let roster = host
        .callbacks
        .maybe_roster
        .as_ref()
        .ok_or("the page gives no roster")?;
      let id = serde_json::to_string(&conversation).map_err(|error| error.to_string())?;
      let answer = call_promise(roster, &JsValue::from_str(&id))
        .await
        .map_err(|failure| failure.to_wire())?;
      let text = answer.as_string().ok_or("the roster is no text")?;
      serde_json::from_str(&text).map_err(|error| error.to_string())
    })
  }
}

/// What goes behind the camera: the background shown (the call state's), the pipeline's effect
/// (none: the camera as it is) and whether the organization requires one.
struct BackgroundPlan {
  shown: BackgroundEffect,
  maybe_effect: Option<MediaEffect>,
  is_required: bool,
}

impl BackgroundPlan {
  fn plain(is_required: bool) -> Self {
    Self {
      shown: BackgroundEffect::None,
      maybe_effect: None,
      is_required,
    }
  }
}

impl State {
  /// The camera stopped: its streams end and the self view has nothing to show.
  fn release_camera(&mut self) {
    for stream in [self.maybe_local_camera.take(), self.maybe_effects_view.take()]
      .into_iter()
      .flatten()
    {
      stop_tracks(&stream);
    }
    self.is_camera_through_effects = false;
  }

  /// What the self view plays: what the effects send, else the camera itself.
  fn shown_camera(&self) -> Option<&MediaStream> {
    self
      .maybe_effects_view
      .as_ref()
      .or(self.maybe_local_camera.as_ref())
  }
}

/// A rule that only says whether a background is required (an older page's preparation).
fn required_only(is_required: bool) -> OrgBackgrounds {
  OrgBackgrounds {
    mode: if is_required {
      teamcraft_proto::CallBackgroundMode::Required
    } else {
      teamcraft_proto::CallBackgroundMode::Free
    },
    applies_to_me: is_required,
    ..OrgBackgrounds::default()
  }
}

fn background_required(detail: &str) -> ActionFailure {
  ActionFailure::new(
    FailureKind::Call(CallFailure::BackgroundRequired),
    format!("the organization requires a background: {detail}"),
  )
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
    (MediaInput::Screen, MediaAccessReason::NoUserActivation) => CallFailure::ScreenNeedsClick,
    (_, MediaAccessReason::Other | MediaAccessReason::NoUserActivation) => return None,
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
