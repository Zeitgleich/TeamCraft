//! The page's service worker container, where the browser offers one.
//!
//! `navigator.serviceWorker` is missing in Firefox with service workers turned off
//! (`dom.serviceWorkers.enabled`, as Tor Browser, Mullvad Browser and hardened profiles do), in
//! private windows of browsers without service workers there, and outside a secure context.
//! web-sys's getter then hands back `undefined` typed as a container, and the first method called
//! on it throws: at the start that stopped the app before it mounted, so a meeting link opened only
//! a blank page (#1721). Every use goes through [`container`], and the app runs without a worker
//! (no install, no pushes, no release check), as it does on plain http.

use wasm_bindgen::{JsCast, JsValue};

/// `navigator.serviceWorker`, or `None` where the browser has none.
pub fn container() -> Option<web_sys::ServiceWorkerContainer> {
  let navigator = web_sys::window()?.navigator();
  let value = js_sys::Reflect::get(&navigator, &JsValue::from_str("serviceWorker")).ok()?;
  // An object test, not `instanceof`: the interface object itself is gone where the feature is off.
  value.is_object().then(|| value.unchecked_into())
}
