//! Calls in the browser (#330): what the desktop shell's call host does, in the page.
//!
//! [`WebCallHost`] keeps the call state the UI shows ([`CallSnapshot`]): the roster and ringing
//! from the client core's gateway events, and the active call from the call module. The call
//! module (`teamcraft-web-calls`) runs the media; it is a wasm module of its own that is loaded on
//! the first call, so the call engine doesn't add to the app's download (`page_calls` has the
//! contract). The device key stays here: the module asks the page to sign this device's call key
//! and to verify the others' (e2ee.md §8).
//!
//! Calls run in the tab that runs the client core ([`crate::web_core::runs_here`]); other tabs of
//! the same account say so instead of offering calls.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::{LocalBoxFuture, Shared};
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use teamcraft_client_api::call_view::{CallView, MediaCallState};
use teamcraft_client_api::calls::{
  CallRequest, CallStatusSounds, CallsUnavailableReason, MediaDevices, PushToTalk,
};
use teamcraft_client_api::failure::{ActionFailure, CallFailure, FailureKind};
use teamcraft_client_api::page_calls::{
  EXPORT_ACT, EXPORT_DEVICES, EXPORT_JOIN, EXPORT_LOCAL_CAMERA, EXPORT_START, EXPORT_STOP,
  EXPORT_UNWATCH_VIDEO, EXPORT_WATCH_VIDEO, MODULE_URL, WebCallJoin,
};
use teamcraft_client_api::remote::{CallAction, CallSnapshot, ChatAnswer, ChatQuery};
use teamcraft_core::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::call_auth::{CallBindingSigner, CallKeyAuthenticator, DeviceCallKeyAuthenticator};
use teamcraft_proto::{CallId, ConversationId, DeviceId, GatewayEvent, MediaStreamId};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};
use web_sys::{HtmlCanvasElement, MediaStream};

/// What a browser needs for calls (as `teamcraft_media::call_web` checks it).
const REQUIRED_GLOBALS: [&str; 4] = ["WebTransport", "AudioEncoder", "AudioDecoder", "AudioWorkletNode"];

#[wasm_bindgen(inline_js = "export function import_module(url) { return import(url); }")]
extern "C" {
  /// `import(url)`: loads an ES module of the page's own origin.
  fn import_module(url: &str) -> Promise;
}

thread_local! {
  static HOST: RefCell<Weak<WebCallHost>> = const { RefCell::new(Weak::new()) };
}

type ChangeHandler = Rc<dyn Fn(CallSnapshot)>;
type ModuleLoad = Shared<LocalBoxFuture<'static, Result<CallModule, String>>>;

/// The calls of the signed-in account in this page.
pub struct WebCallHost {
  state: RefCell<HostState>,
  maybe_on_change: RefCell<Option<ChangeHandler>>,
  maybe_module: RefCell<Option<ModuleLoad>>,
  /// The call being joined or running: who may be in it, and the key that signs for this device.
  maybe_call: RefCell<Option<PreparedCall>>,
}

#[derive(Default)]
struct HostState {
  view: CallView,
  devices: MediaDevices,
  push_to_talk: PushToTalk,
  status_sounds: CallStatusSounds,
  maybe_microphone: Option<String>,
  maybe_speakers: Option<String>,
}

struct PreparedCall {
  conversation: ConversationId,
  authenticator: DeviceCallKeyAuthenticator,
}

/// The loaded call module and the callbacks it holds (kept alive with it).
#[derive(Clone)]
struct CallModule {
  exports: JsValue,
  _callbacks: Rc<Callbacks>,
}

struct Callbacks {
  object: Object,
  _changed: Closure<dyn Fn(String)>,
  _sign: Closure<dyn Fn(String, Uint8Array) -> Result<Uint8Array, JsValue>>,
  _verify: Closure<dyn Fn(String, String, Uint8Array) -> Result<Uint8Array, JsValue>>,
  _ticket: Closure<dyn Fn() -> Promise>,
}

/// Lets the media layer's authenticator sign with the core's signer.
struct CoreSigner(CallKeySigner);

/// Whether this page can make calls, and why not.
pub fn support() -> Result<(), CallsUnavailableReason> {
  let global = js_sys::global();
  let has_all = REQUIRED_GLOBALS
    .iter()
    .all(|name| Reflect::has(&global, &JsValue::from_str(name)).unwrap_or(false));
  if !has_all {
    return Err(CallsUnavailableReason::BrowserUnsupported);
  }
  if !crate::web_core::runs_here() {
    return Err(CallsUnavailableReason::OtherTab);
  }
  Ok(())
}

/// The client core's call events (ringing, rosters) for the page's calls.
pub fn on_gateway_event(event: &GatewayEvent) {
  if let Some(host) = current() {
    host.on_gateway_event(event);
  }
}

/// Whether the page shows real call video (a call module is loaded).
pub fn can_show_video() -> bool {
  current().is_some_and(|host| host.loaded_module().is_some())
}

/// Draws another device's track into `canvas` until [`unwatch_video`].
pub fn watch_video(stream_id: MediaStreamId, canvas: &HtmlCanvasElement) {
  if let Some(module) = current().and_then(|host| host.loaded_module())
    && let Ok(id) = serde_json::to_string(&stream_id)
  {
    module.call_sync(EXPORT_WATCH_VIDEO, &[JsValue::from_str(&id), canvas.into()]);
  }
}

pub fn unwatch_video(stream_id: MediaStreamId) {
  if let Some(module) = current().and_then(|host| host.loaded_module())
    && let Ok(id) = serde_json::to_string(&stream_id)
  {
    module.call_sync(EXPORT_UNWATCH_VIDEO, &[JsValue::from_str(&id)]);
  }
}

/// The camera this device sends, for the self view.
pub fn local_camera() -> Option<MediaStream> {
  let module = current()?.loaded_module()?;
  module
    .call_sync(EXPORT_LOCAL_CAMERA, &[])
    .and_then(|stream| stream.dyn_into().ok())
}

fn current() -> Option<Rc<WebCallHost>> {
  HOST.with(|host| host.borrow().upgrade())
}

impl WebCallHost {
  /// The host of this page's calls; the gateway events of the core reach the newest one.
  pub fn new() -> Rc<Self> {
    let host = Rc::new(Self {
      state: RefCell::new(HostState::default()),
      maybe_on_change: RefCell::new(None),
      maybe_module: RefCell::new(None),
      maybe_call: RefCell::new(None),
    });
    HOST.with(|slot| *slot.borrow_mut() = Rc::downgrade(&host));
    host
  }

  pub fn set_on_change(&self, on_change: ChangeHandler) {
    *self.maybe_on_change.borrow_mut() = Some(on_change);
  }

  pub fn snapshot(&self) -> CallSnapshot {
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
  }

  /// Carries out one of the user's call actions; a failure comes back as `ActionFailure` wire text.
  pub async fn act(self: &Rc<Self>, action: CallAction) -> Result<(), String> {
    self.act_inner(action).await.map_err(|failure| failure.to_wire())
  }

  async fn act_inner(self: &Rc<Self>, action: CallAction) -> Result<(), ActionFailure> {
    match action {
      CallAction::Join { request } => self.join(request).await,
      CallAction::Leave => {
        self.forward(&CallAction::Leave).await?;
        self.maybe_call.borrow_mut().take();
        Ok(())
      },
      CallAction::Decline { conversation } => {
        self.state.borrow_mut().view.decline(conversation);
        self.changed();
        Ok(())
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
        self.changed();
        self.forward(&CallAction::SetMuted { is_muted: is_enabled }).await
      },
      CallAction::SetTalking { is_talking } => {
        {
          let mut state = self.state.borrow_mut();
          if !state.push_to_talk.is_enabled || state.push_to_talk.is_talking == is_talking {
            return Ok(());
          }
          state.push_to_talk.is_talking = is_talking;
        }
        self.changed();
        self
          .forward(&CallAction::SetMuted {
            is_muted: !is_talking,
          })
          .await
      },
      CallAction::SelectMicrophone { maybe_device } => {
        self.state.borrow_mut().maybe_microphone = maybe_device.clone();
        self.forward(&CallAction::SelectMicrophone { maybe_device }).await
      },
      CallAction::SelectSpeakers { maybe_device } => {
        self.state.borrow_mut().maybe_speakers = maybe_device.clone();
        self.forward(&CallAction::SelectSpeakers { maybe_device }).await
      },
      CallAction::SetStatusSounds { sounds } => {
        self.state.borrow_mut().status_sounds = sounds;
        self.forward(&CallAction::SetStatusSounds { sounds }).await
      },
      CallAction::SetCamera { is_on: true } => {
        // admin.md §3.5: the browser can't put a background behind the camera image, so where the
        // organization requires one, the camera stays off.
        if org_requires_background().await {
          return Err(ActionFailure::new(
            FailureKind::Call(CallFailure::BackgroundRequired),
            "the organization requires a background, which the browser can't apply",
          ));
        }
        self.forward(&CallAction::SetCamera { is_on: true }).await
      },
      // The browser has no background effects (the UI offers none) and no system effects panel.
      CallAction::SetBackgroundEffect { .. } | CallAction::OpenSystemVideoEffects => Ok(()),
      CallAction::Unknown => Err(ActionFailure::other("unknown call action")),
      other => self.forward(&other).await,
    }
  }

  async fn join(self: &Rc<Self>, request: CallRequest) -> Result<(), ActionFailure> {
    let module = self.module().await.map_err(ActionFailure::other)?;
    // Leave the running call first: its end must not clear the keys of the new one.
    self.forward(&CallAction::Leave).await?;
    self.maybe_call.borrow_mut().take();
    let preparation = crate::web_core::prepare_call(request.conversation)
      .await
      .map_err(|text| ActionFailure::from_wire(&text))?;
    let own_device = preparation.device_id;
    let authenticator = DeviceCallKeyAuthenticator::from_verified(
      Arc::new(CoreSigner(preparation.signer)),
      preparation.devices,
    )
    .map_err(|error| ActionFailure::other(error.to_string()))?;
    let trusted_devices: Vec<DeviceId> = authenticator
      .device_ids()
      .filter(|device| *device != own_device)
      .collect();
    *self.maybe_call.borrow_mut() = Some(PreparedCall {
      conversation: request.conversation,
      authenticator,
    });
    let (join, is_push_to_talk) = {
      let state = self.state.borrow();
      let join = WebCallJoin {
        request,
        sfu_url: preparation.join.sfu_url,
        token: preparation.join.token,
        user_id: preparation.user_id,
        device_id: own_device,
        trusted_devices,
        maybe_microphone: state.maybe_microphone.clone(),
        maybe_speakers: state.maybe_speakers.clone(),
        status_sounds: state.status_sounds,
      };
      (join, state.push_to_talk.is_enabled)
    };
    let json = serde_json::to_string(&join).map_err(|error| ActionFailure::other(error.to_string()))?;
    module.call(EXPORT_JOIN, &[JsValue::from_str(&json)]).await?;
    if is_push_to_talk {
      // Push-to-talk: the microphone opens only while the key is held.
      self.forward(&CallAction::SetMuted { is_muted: true }).await?;
    }
    // With the microphone allowed, the browser names its devices.
    self.refresh_devices();
    Ok(())
  }

  /// Passes an action to the call module; before it is loaded there is no call to change.
  async fn forward(&self, action: &CallAction) -> Result<(), ActionFailure> {
    let Some(module) = self.loaded_module() else {
      return Ok(());
    };
    let json = serde_json::to_string(action).map_err(|error| ActionFailure::other(error.to_string()))?;
    module.call(EXPORT_ACT, &[JsValue::from_str(&json)]).await?;
    Ok(())
  }

  /// The call module, loading it on first use.
  async fn module(self: &Rc<Self>) -> Result<CallModule, String> {
    let load = {
      let mut slot = self.maybe_module.borrow_mut();
      slot
        .get_or_insert_with(|| load_module(Rc::downgrade(self)).boxed_local().shared())
        .clone()
    };
    let result = load.await;
    if result.is_err() {
      // Try again on the next call (a network hiccup while loading).
      self.maybe_module.borrow_mut().take();
    } else {
      self.refresh_devices_once();
    }
    result
  }

  fn loaded_module(&self) -> Option<CallModule> {
    let load = self.maybe_module.borrow().clone()?;
    load.peek().and_then(|result| result.as_ref().ok().cloned())
  }

  fn refresh_devices_once(self: &Rc<Self>) {
    let state = self.state.borrow();
    let devices = &state.devices;
    if devices.microphones.is_empty() && devices.speakers.is_empty() && devices.cameras.is_empty() {
      drop(state);
      self.refresh_devices();
    }
  }

  /// Lists the browser's devices again (they come and go, and get names with permission).
  fn refresh_devices(self: &Rc<Self>) {
    let Some(module) = self.loaded_module() else {
      return;
    };
    let host = Rc::downgrade(self);
    spawn_local(async move {
      let devices = match module.call(EXPORT_DEVICES, &[]).await {
        Ok(answer) => answer
          .as_string()
          .and_then(|text| serde_json::from_str::<MediaDevices>(&text).ok()),
        Err(failure) => {
          tracing::debug!(detail = %failure.detail, "could not list the media devices");
          None
        },
      };
      if let (Some(host), Some(devices)) = (host.upgrade(), devices) {
        host.state.borrow_mut().devices = devices;
        host.changed();
      }
    });
  }

  fn on_gateway_event(&self, event: &GatewayEvent) {
    let is_changed = self.state.borrow_mut().view.apply_gateway_event(event);
    if is_changed {
      self.changed();
    }
  }

  fn on_media_state(&self, json: &str) {
    match serde_json::from_str::<MediaCallState>(json) {
      Ok(media) => {
        let has_ended = media.maybe_active.is_none();
        self.state.borrow_mut().view.set_media_state(media);
        if has_ended {
          self.maybe_call.borrow_mut().take();
        }
        self.changed();
      },
      Err(error) => tracing::warn!(%error, "unreadable call state from the call module"),
    }
  }

  fn sign(&self, call_id: &str, dave_public_key: &[u8]) -> Result<Vec<u8>, String> {
    let call_id: CallId = serde_json::from_str(call_id).map_err(|error| error.to_string())?;
    let call = self.maybe_call.borrow();
    let call = call.as_ref().ok_or("no call is being joined")?;
    call
      .authenticator
      .sign(call_id, dave_public_key)
      .map_err(|error| error.to_string())
  }

  fn verify(&self, call_id: &str, device_id: &str, binding: &[u8]) -> Result<Vec<u8>, String> {
    let call_id: CallId = serde_json::from_str(call_id).map_err(|error| error.to_string())?;
    let device_id: DeviceId = serde_json::from_str(device_id).map_err(|error| error.to_string())?;
    let call = self.maybe_call.borrow();
    let call = call.as_ref().ok_or("no call is being joined")?;
    call
      .authenticator
      .verify(call_id, device_id, binding)
      .map_err(|error| error.to_string())
  }

  fn call_conversation(&self) -> Option<ConversationId> {
    self.maybe_call.borrow().as_ref().map(|call| call.conversation)
  }

  fn changed(&self) {
    let snapshot = self.snapshot();
    let maybe_on_change = self.maybe_on_change.borrow().clone();
    if let Some(on_change) = maybe_on_change {
      on_change(snapshot);
    }
  }
}

/// Signing out: a running call ends with the page's session.
impl Drop for WebCallHost {
  fn drop(&mut self) {
    if let Some(module) = self.loaded_module() {
      // The module keeps calling back while it leaves; the callbacks live until it is done.
      spawn_local(async move {
        if let Err(failure) = module.call(EXPORT_STOP, &[]).await {
          tracing::debug!(detail = %failure.detail, "the call module did not stop cleanly");
        }
      });
    }
  }
}

impl CallModule {
  /// Calls an async export; a rejection is `ActionFailure` wire text.
  async fn call(&self, name: &str, args: &[JsValue]) -> Result<JsValue, ActionFailure> {
    let answer = self
      .function(name)?
      .apply(&JsValue::NULL, &args.iter().collect::<Array>())
      .map_err(|error| ActionFailure::other(js_text(&error)))?;
    let promise: Promise = answer
      .dyn_into()
      .map_err(|_| ActionFailure::other(format!("{name} of the call module answered no promise")))?;
    JsFuture::from(promise)
      .await
      .map_err(|error| ActionFailure::from_wire(&js_text(&error)))
  }

  fn call_sync(&self, name: &str, args: &[JsValue]) -> Option<JsValue> {
    let function = self.function(name).ok()?;
    function
      .apply(&JsValue::NULL, &args.iter().collect::<Array>())
      .inspect_err(|error| tracing::debug!(name, error = %js_text(error), "call module export failed"))
      .ok()
  }

  fn function(&self, name: &str) -> Result<Function, ActionFailure> {
    Reflect::get(&self.exports, &JsValue::from_str(name))
      .ok()
      .and_then(|value| value.dyn_into().ok())
      .ok_or_else(|| ActionFailure::other(format!("the call module has no {name}")))
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

/// Imports and initializes the call module and hands it the page's callbacks.
async fn load_module(host: Weak<WebCallHost>) -> Result<CallModule, String> {
  let exports = JsFuture::from(import_module(MODULE_URL))
    .await
    .map_err(|error| format!("could not load the call module: {}", js_text(&error)))?;
  let init: Function = Reflect::get(&exports, &JsValue::from_str("default"))
    .ok()
    .and_then(|value| value.dyn_into().ok())
    .ok_or("the call module has no initializer")?;
  let started = init
    .call0(&JsValue::NULL)
    .map_err(|error| js_text(&error))?
    .dyn_into::<Promise>()
    .map_err(|_| "the call module's initializer answered no promise".to_owned())?;
  JsFuture::from(started)
    .await
    .map_err(|error| format!("could not start the call module: {}", js_text(&error)))?;
  let callbacks = Rc::new(Callbacks::new(host)?);
  let module = CallModule {
    exports,
    _callbacks: callbacks.clone(),
  };
  module
    .function(EXPORT_START)
    .map_err(|failure| failure.detail)?
    .call1(&JsValue::NULL, &callbacks.object)
    .map_err(|error| js_text(&error))?;
  tracing::info!("call module loaded");
  Ok(module)
}

impl Callbacks {
  fn new(host: Weak<WebCallHost>) -> Result<Self, String> {
    let changed = {
      let host = host.clone();
      Closure::<dyn Fn(String)>::new(move |json: String| {
        if let Some(host) = host.upgrade() {
          host.on_media_state(&json);
        }
      })
    };
    let sign = {
      let host = host.clone();
      Closure::<dyn Fn(String, Uint8Array) -> Result<Uint8Array, JsValue>>::new(
        move |call_id: String, key: Uint8Array| {
          let host = host.upgrade().ok_or_else(|| JsValue::from_str("signed out"))?;
          let signed = host
            .sign(&call_id, &key.to_vec())
            .map_err(|error| JsValue::from_str(&error))?;
          Ok(Uint8Array::from(signed.as_slice()))
        },
      )
    };
    let verify = {
      let host = host.clone();
      Closure::<dyn Fn(String, String, Uint8Array) -> Result<Uint8Array, JsValue>>::new(
        move |call_id: String, device_id: String, binding: Uint8Array| {
          let host = host.upgrade().ok_or_else(|| JsValue::from_str("signed out"))?;
          let key = host
            .verify(&call_id, &device_id, &binding.to_vec())
            .map_err(|error| JsValue::from_str(&error))?;
          Ok(Uint8Array::from(key.as_slice()))
        },
      )
    };
    let ticket = Closure::<dyn Fn() -> Promise>::new(move || {
      let maybe_conversation = host.upgrade().and_then(|host| host.call_conversation());
      future_to_promise(async move {
        let conversation = maybe_conversation.ok_or_else(|| JsValue::from_str("no call to rejoin"))?;
        let token = crate::web_core::call_join_token(conversation)
          .await
          .map_err(|text| JsValue::from_str(&text))?;
        let json = serde_json::to_string(&token).map_err(|error| JsValue::from_str(&error.to_string()))?;
        Ok(JsValue::from_str(&json))
      })
    });
    let object = Object::new();
    for (name, function) in [
      ("changed", changed.as_ref()),
      ("sign", sign.as_ref()),
      ("verify", verify.as_ref()),
      ("ticket", ticket.as_ref()),
    ] {
      Reflect::set(&object, &JsValue::from_str(name), function).map_err(|error| js_text(&error))?;
    }
    Ok(Self {
      object,
      _changed: changed,
      _sign: sign,
      _verify: verify,
      _ticket: ticket,
    })
  }
}

/// Whether the workspace policy requires an organization background behind the camera image
/// (and it applies to this user). Unknown policy restricts nothing, as on desktop.
async fn org_requires_background() -> bool {
  let query = serde_json::json!({ "query": ChatQuery::OrgBackgrounds });
  match crate::web_core::call("core_query", query).await {
    Ok(answer) => matches!(
      serde_json::from_value::<ChatAnswer>(answer),
      Ok(ChatAnswer::OrgBackgrounds { backgrounds }) if backgrounds.is_required()
    ),
    Err(error) => {
      tracing::debug!(%error, "workspace backgrounds unknown");
      false
    },
  }
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
