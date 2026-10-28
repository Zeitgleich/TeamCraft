//! Calls in the browser (#330): the page's half.
//!
//! The calls themselves (call state, ringtone, media) are a wasm module of their own,
//! `teamcraft-web-calls`, which [`WebCallHost`] loads in the background once the user is signed in,
//! so it doesn't count against the app's first download. The page keeps what needs the client core:
//! it forwards the gateway's call events, prepares joins, and signs this device's call key and
//! verifies the others' with the device key, which never leaves the core (e2ee.md §8). The contract
//! is `teamcraft_client_api::page_calls`.
//!
//! Calls run in the tab that runs the client core ([`crate::web_core::runs_here`]); other tabs of
//! the same account say so instead of offering calls.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::{LocalBoxFuture, Shared};
use js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use teamcraft_client_api::call_view::CallSignal;
use teamcraft_client_api::calls::CallsUnavailableReason;
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_client_api::page_calls::{
  EXPORT_ACT, EXPORT_CAMERA_VIEW, EXPORT_NETWORK_CHANGED, EXPORT_PICK_SCREEN, EXPORT_PREVIEW_LEVEL,
  EXPORT_RING, EXPORT_SHARE_SCREEN, EXPORT_SIGNAL, EXPORT_START, EXPORT_STOP, EXPORT_UNWATCH_VIDEO,
  EXPORT_VIDEO_STATS, EXPORT_WATCH_VIDEO, MODULE_META, PreparedCall, WORKLET_META,
};
use teamcraft_client_api::remote::{CallAction, CallActionAnswer, CallSnapshot};
use teamcraft_core::CallKeySigner;
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_media::call_auth::{CallBindingSigner, CallKeyAuthenticator, DeviceCallKeyAuthenticator};
use teamcraft_proto::{CallId, ConversationId, DeviceId, GatewayEvent, MediaStreamId, ScreenContentMode};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};
use web_sys::Element;

/// What a browser needs for calls (as `teamcraft_media::call_web` checks it).
const REQUIRED_GLOBALS: [&str; 4] = ["WebTransport", "AudioEncoder", "AudioDecoder", "AudioWorkletNode"];
/// Gateway call events kept while the module loads; older ones go first (later ones repeat the state).
const MAX_PENDING_SIGNALS: usize = 64;
/// The call module loads this long after sign-in, out of the way of the first screen.
const LOAD_DELAY: Duration = Duration::from_secs(3);

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

/// The page's side of the calls of the signed-in account.
pub struct WebCallHost {
  maybe_on_change: RefCell<Option<ChangeHandler>>,
  maybe_module: RefCell<Option<ModuleLoad>>,
  /// Gateway call events that arrived before the module ran, in order.
  pending_signals: RefCell<Vec<String>>,
  is_ringing: Cell<bool>,
  /// Whether the last snapshot had a call this device is in.
  was_in_call: Cell<bool>,
  /// The call being joined or running: who may be in it, and the key that signs for this device.
  maybe_call: RefCell<Option<PreparedKeys>>,
}

struct PreparedKeys {
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
  _prepare: Closure<dyn Fn(String) -> Promise>,
  _sign: Closure<dyn Fn(String, Uint8Array) -> Result<Uint8Array, JsValue>>,
  _verify: Closure<dyn Fn(String, String, Uint8Array) -> Result<Uint8Array, JsValue>>,
  _failed: Closure<dyn Fn(String)>,
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

/// Whether this page is in a call. Another tab's takeover waits for it (#744).
pub fn is_in_call() -> bool {
  current().is_some_and(|host| host.was_in_call.get())
}

/// Leaves the call this page is in, before another tab takes over (the user agreed).
pub async fn end_call() {
  let Some(module) = current().and_then(|host| host.loaded_module()) else {
    return;
  };
  if let Err(failure) = module.call(EXPORT_STOP, &[]).await {
    tracing::debug!(detail = %failure.detail, "the call module did not stop cleanly");
  }
}

/// The client core's call events (ringing, rosters) for the call module.
pub fn on_gateway_event(event: &GatewayEvent) {
  if let (Some(host), Some(signal)) = (current(), CallSignal::from_gateway(event)) {
    host.signal(&signal);
  }
}

/// Starts or stops the ringtone.
pub fn set_ringing(is_ringing: bool) {
  if let Some(host) = current() {
    host.is_ringing.set(is_ringing);
    if let Some(module) = host.loaded_module() {
      module.call_sync(EXPORT_RING, &[JsValue::from_bool(is_ringing)]);
    }
  }
}

/// The core's network-change signal, for a running call: it checks its session at once instead of
/// waiting for QUIC's idle timeout, or retries a rejoin. Without the module there is no call.
pub fn network_changed() {
  if let Some(module) = current().and_then(|host| host.loaded_module()) {
    module.call_sync(EXPORT_NETWORK_CHANGED, &[]);
  }
}

/// Whether this page runs the calls itself (the browser, not the desktop shell).
pub fn is_in_page() -> bool {
  current().is_some()
}

/// Whether the page shows real call video (the call module runs).
pub fn can_show_video() -> bool {
  current().is_some_and(|host| host.loaded_module().is_some())
}

/// A new element showing another device's track until [`unwatch_video`].
pub fn watch_video(stream_id: MediaStreamId) -> Option<Element> {
  let module = current()?.loaded_module()?;
  module
    .call_sync(EXPORT_WATCH_VIDEO, &[json_value(&stream_id)])
    .and_then(|canvas| canvas.dyn_into().ok())
}

pub fn unwatch_video(stream_id: MediaStreamId) {
  if let Some(module) = current().and_then(|host| host.loaded_module()) {
    module.call_sync(EXPORT_UNWATCH_VIDEO, &[json_value(&stream_id)]);
  }
}

/// A new element playing the camera this device sends, for the self view.
pub fn camera_view() -> Option<Element> {
  let module = current()?.loaded_module()?;
  module
    .call_sync(EXPORT_CAMERA_VIEW, &[])
    .and_then(|video| video.dyn_into().ok())
}

/// Opens the browser's screen picker now and returns the promise of the chosen screen, for
/// [`WebCallHost::share_screen`]. Call it synchronously in the click handler: Safari opens the
/// picker only while the click's user activation lasts, which the action queue outlasts (issue
/// #930). `None` without a running call module, or when the browser can't open a picker; the share
/// then goes the usual way, which says why it failed.
pub fn pick_screen(mode: ScreenContentMode) -> Option<Promise> {
  let module = current()?.loaded_module()?;
  module
    .call_sync(EXPORT_PICK_SCREEN, &[json_value(&mode)])
    .and_then(|picked| picked.dyn_into().ok())
}

/// The microphone's level in the pre-join check (#442), from 0.0 to 1.0; 0.0 without one.
pub fn preview_level() -> f32 {
  current()
    .and_then(|host| host.loaded_module())
    .and_then(|module| module.call_sync(EXPORT_PREVIEW_LEVEL, &[]))
    .and_then(|level| level.as_f64())
    .map_or(0.0, |level| level as f32)
}

/// What the call's video does now (stats for nerds, #938), as the module's `VideoStatsReport`
/// JSON; `None` outside a call or before the call module loaded.
pub async fn video_stats() -> Option<String> {
  let module = current()?.loaded_module()?;
  module.call(EXPORT_VIDEO_STATS, &[]).await.ok()?.as_string()
}

fn current() -> Option<Rc<WebCallHost>> {
  HOST.with(|host| host.borrow().upgrade())
}

impl WebCallHost {
  /// The calls of this page; the module loads shortly after.
  pub fn new() -> Rc<Self> {
    let host = Rc::new(Self {
      maybe_on_change: RefCell::new(None),
      maybe_module: RefCell::new(None),
      pending_signals: RefCell::new(Vec::new()),
      is_ringing: Cell::new(false),
      was_in_call: Cell::new(false),
      maybe_call: RefCell::new(None),
    });
    HOST.with(|slot| *slot.borrow_mut() = Rc::downgrade(&host));
    let weak = Rc::downgrade(&host);
    spawn_local(async move {
      teamcraft_core::platform::sleep(LOAD_DELAY).await;
      if let Some(host) = weak.upgrade()
        && let Err(error) = host.module().await
      {
        // Tried again with the next call event or action.
        tracing::warn!(%error, "the call module did not load");
      }
    });
    host
  }

  /// Gets every new call state of the module.
  pub fn set_on_change(&self, on_change: ChangeHandler) {
    *self.maybe_on_change.borrow_mut() = Some(on_change);
  }

  /// Carries out one of the user's call actions; a failure comes back as `ActionFailure` wire text,
  /// and so does a part of it that failed without undoing it (the answer).
  pub async fn act(self: &Rc<Self>, action: CallAction) -> Result<CallActionAnswer, String> {
    let module = self.module().await?;
    let json = serde_json::to_string(&action).map_err(|error| error.to_string())?;
    module
      .call(EXPORT_ACT, &[JsValue::from_str(&json)])
      .await
      .map(|answer| answer.as_string())
      .map_err(|failure| failure.to_wire())
  }

  /// Shares the screen chosen in a picker [`pick_screen`] opened (`StartScreenShare` with the
  /// screen already asked for); answers like [`WebCallHost::act`].
  pub async fn share_screen(
    self: &Rc<Self>,
    has_sound: bool,
    mode: ScreenContentMode,
    picked: Promise,
  ) -> Result<CallActionAnswer, String> {
    let module = self.module().await?;
    module
      .call(
        EXPORT_SHARE_SCREEN,
        &[JsValue::from_bool(has_sound), json_value(&mode), picked.into()],
      )
      .await
      .map(|answer| answer.as_string())
      .map_err(|failure| failure.to_wire())
  }

  fn signal(self: &Rc<Self>, signal: &CallSignal) {
    let Ok(json) = serde_json::to_string(signal) else {
      return;
    };
    match self.loaded_module() {
      Some(module) => {
        module.call_sync(EXPORT_SIGNAL, &[JsValue::from_str(&json)]);
      },
      None => {
        let mut pending = self.pending_signals.borrow_mut();
        if pending.len() >= MAX_PENDING_SIGNALS {
          pending.remove(0);
        }
        pending.push(json);
        drop(pending);
        let host = self.clone();
        spawn_local(async move {
          if let Err(error) = host.module().await {
            tracing::warn!(%error, "the call module did not load");
          }
        });
      },
    }
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
    match &result {
      // A network hiccup while loading: the next use tries again.
      Err(_) => drop(self.maybe_module.borrow_mut().take()),
      Ok(module) => self.flush_pending(module),
    }
    result
  }

  /// What arrived while the module loaded.
  fn flush_pending(&self, module: &CallModule) {
    let signals = std::mem::take(&mut *self.pending_signals.borrow_mut());
    for json in signals {
      module.call_sync(EXPORT_SIGNAL, &[JsValue::from_str(&json)]);
    }
    if self.is_ringing.get() {
      module.call_sync(EXPORT_RING, &[JsValue::TRUE]);
    }
  }

  fn loaded_module(&self) -> Option<CallModule> {
    let load = self.maybe_module.borrow().clone()?;
    load.peek().and_then(|result| result.as_ref().ok().cloned())
  }

  fn on_snapshot(&self, json: &str) {
    match serde_json::from_str::<CallSnapshot>(json) {
      Ok(snapshot) => {
        let is_in_call = snapshot.maybe_active.is_some();
        if self.was_in_call.replace(is_in_call) && !is_in_call {
          // The call ended: its keys go. (A join in progress has no active call yet.)
          self.maybe_call.borrow_mut().take();
        }
        let maybe_on_change = self.maybe_on_change.borrow().clone();
        if let Some(on_change) = maybe_on_change {
          on_change(snapshot);
        }
      },
      Err(error) => tracing::warn!(%error, "unreadable call state from the call module"),
    }
  }

  /// Asks the core for a join token and the verified devices, and keeps the keys for the call.
  async fn prepare(&self, conversation: ConversationId) -> Result<PreparedCall, ActionFailure> {
    let preparation = crate::web_core::prepare_call(conversation)
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
    *self.maybe_call.borrow_mut() = Some(PreparedKeys { authenticator });
    Ok(PreparedCall {
      sfu_url: preparation.join.sfu_url,
      token: preparation.join.token,
      user_id: preparation.user_id,
      device_id: own_device,
      trusted_devices,
      requires_background: crate::web_core::requires_org_background().await,
      certificate_sha256: preparation.join.sfu_certificate_sha256,
    })
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
  let module_url = meta_content(MODULE_META).ok_or("the page names no call module")?;
  let exports = JsFuture::from(import_module(&module_url))
    .await
    .map_err(|error| format!("could not load the call module: {}", js_text(&error)))?;
  let init: Function = Reflect::get(&exports, &JsValue::from_str("default"))
    .ok()
    .and_then(|value| value.dyn_into().ok())
    .ok_or("the call module has no initializer")?;
  let started: Promise = init
    .call0(&JsValue::NULL)
    .and_then(JsCast::dyn_into)
    .map_err(|error| js_text(&error))?;
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
  Ok(module)
}

impl Callbacks {
  fn new(host: Weak<WebCallHost>) -> Result<Self, String> {
    let changed = {
      let host = host.clone();
      Closure::<dyn Fn(String)>::new(move |json: String| {
        if let Some(host) = host.upgrade() {
          host.on_snapshot(&json);
        }
      })
    };
    let prepare = {
      let host = host.clone();
      Closure::<dyn Fn(String) -> Promise>::new(move |conversation: String| {
        let host = host.clone();
        future_to_promise(async move {
          let rejected = |failure: ActionFailure| JsValue::from_str(&failure.to_wire());
          let conversation: ConversationId = serde_json::from_str(&conversation)
            .map_err(|error| rejected(ActionFailure::other(error.to_string())))?;
          let host = host
            .upgrade()
            .ok_or_else(|| rejected(ActionFailure::other("signed out")))?;
          let prepared = host.prepare(conversation).await.map_err(rejected)?;
          serde_json::to_string(&prepared)
            .map(|json| JsValue::from_str(&json))
            .map_err(|error| rejected(ActionFailure::other(error.to_string())))
        })
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
    // A failure no action waits for (a camera that stopped mid-call), as `ActionFailure` wire text.
    let failed = Closure::<dyn Fn(String)>::new(|wire: String| {
      crate::feedback::report_failure(ActionFailure::from_wire(&wire));
    });
    let object = Object::new();
    let worklet = meta_content(WORKLET_META).ok_or("the page names no call worklet")?;
    Reflect::set(
      &object,
      &JsValue::from_str("worklet"),
      &JsValue::from_str(&worklet),
    )
    .map_err(|error| js_text(&error))?;
    for (name, function) in [
      ("changed", changed.as_ref()),
      ("prepare", prepare.as_ref()),
      ("sign", sign.as_ref()),
      ("verify", verify.as_ref()),
      ("failed", failed.as_ref()),
    ] {
      Reflect::set(&object, &JsValue::from_str(name), function).map_err(|error| js_text(&error))?;
    }
    Ok(Self {
      object,
      _changed: changed,
      _prepare: prepare,
      _sign: sign,
      _verify: verify,
      _failed: failed,
    })
  }
}

/// The `content` of `<meta name="…">` in the page (the build writes the call module's names there).
fn meta_content(name: &str) -> Option<String> {
  web_sys::window()?
    .document()?
    .query_selector(&format!("meta[name=\"{name}\"]"))
    .ok()??
    .get_attribute("content")
}

fn json_value<T: serde::Serialize>(value: &T) -> JsValue {
  // Ids always serialize; an empty string would only match nothing.
  JsValue::from_str(&serde_json::to_string(value).unwrap_or_default())
}

pub(crate) fn js_text(error: &JsValue) -> String {
  error
    .as_string()
    .or_else(|| {
      Reflect::get(error, &JsValue::from_str("message"))
        .ok()
        .and_then(|message| message.as_string())
    })
    .unwrap_or_else(|| format!("{error:?}"))
}
