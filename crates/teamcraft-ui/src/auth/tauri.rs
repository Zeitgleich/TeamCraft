//! Desktop sign-in: the Tauri shell (Rust side) runs the OIDC flow with the system browser and keeps
//! the tokens in the OS keyring; the webview only calls commands and never sees a token.

use std::cell::RefCell;
use std::collections::HashMap;

use serde::Deserialize;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use super::{ApiMethod, ApiResponse, AuthError, Authenticator, LocalFuture};
use crate::model::ConversationId;

/// Whether the page runs inside the desktop shell.
pub fn is_available() -> bool {
  invoke_function().is_some()
}

pub struct TauriAuth;

impl TauriAuth {
  pub fn new() -> Self {
    Self
  }
}

impl Default for TauriAuth {
  fn default() -> Self {
    Self::new()
  }
}

impl Authenticator for TauriAuth {
  fn restore(&self) -> LocalFuture<'_, Result<bool, AuthError>> {
    Box::pin(async {
      let value = invoke("auth_restore", JsValue::UNDEFINED).await?;
      value
        .as_bool()
        .ok_or_else(|| AuthError::InvalidResponse("auth_restore did not return a boolean".to_owned()))
    })
  }

  fn login(&self) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async { invoke("auth_login", JsValue::UNDEFINED).await.map(|_| ()) })
  }

  fn cancel_login(&self) {
    wasm_bindgen_futures::spawn_local(async {
      if let Err(error) = invoke("auth_cancel", JsValue::UNDEFINED).await {
        tracing::debug!(%error, "could not cancel the login");
      }
    });
  }

  fn logout(&self) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async { invoke("auth_logout", JsValue::UNDEFINED).await.map(|_| ()) })
  }

  fn api_request(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<String>,
  ) -> LocalFuture<'_, Result<ApiResponse, AuthError>> {
    let path = path.to_owned();
    Box::pin(async move {
      let invalid = |_| AuthError::InvalidResponse("could not build the request".to_owned());
      let args = js_sys::Object::new();
      let method = serde_json::to_value(method)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default();
      js_sys::Reflect::set(&args, &"method".into(), &method.into()).map_err(invalid)?;
      js_sys::Reflect::set(&args, &"path".into(), &path.into()).map_err(invalid)?;
      if let Some(body) = maybe_body {
        js_sys::Reflect::set(&args, &"body".into(), &body.into()).map_err(invalid)?;
      }
      let value = invoke("api_request", args.into()).await?;
      let response: ApiPayload = serde_wasm_json(&value)?;
      Ok(ApiResponse {
        status: response.status,
        body: response.body,
      })
    })
  }
}

#[derive(Deserialize)]
struct ApiPayload {
  status: u16,
  body: String,
}

/// Error object of the desktop commands: `{ kind, message }`.
#[derive(Deserialize)]
struct CommandError {
  kind: String,
  message: String,
}

pub(crate) async fn invoke(command: &str, args: JsValue) -> Result<JsValue, AuthError> {
  let function =
    invoke_function().ok_or_else(|| AuthError::Unsupported("not running in the desktop app".to_owned()))?;
  let promise = function
    .call2(&JsValue::NULL, &command.into(), &args)
    .map_err(|_| AuthError::Unsupported("the desktop bridge rejected the call".to_owned()))?;
  let promise: js_sys::Promise = promise
    .dyn_into()
    .map_err(|_| AuthError::InvalidResponse("no promise".to_owned()))?;
  JsFuture::from(promise).await.map_err(error_from_js)
}

fn error_from_js(value: JsValue) -> AuthError {
  match serde_wasm_json::<CommandError>(&value) {
    Ok(error) => map_kind(&error.kind, error.message),
    Err(_) => AuthError::InvalidResponse(value.as_string().unwrap_or_else(|| "unknown error".to_owned())),
  }
}

fn map_kind(kind: &str, message: String) -> AuthError {
  match kind {
    "cancelled" => AuthError::Cancelled,
    "denied" => AuthError::Denied(message),
    "network" => AuthError::Network(message),
    "state_mismatch" => AuthError::StateMismatch,
    "not_signed_in" => AuthError::NotSignedIn,
    "unsupported" => AuthError::Unsupported(message),
    _ => AuthError::InvalidResponse(message),
  }
}

/// Converts a JS value to a Rust type through JSON.
fn serde_wasm_json<T: for<'de> Deserialize<'de>>(value: &JsValue) -> Result<T, AuthError> {
  let text = js_sys::JSON::stringify(value)
    .ok()
    .and_then(|text| text.as_string())
    .ok_or_else(|| AuthError::InvalidResponse("not JSON".to_owned()))?;
  serde_json::from_str(&text).map_err(|error| AuthError::InvalidResponse(error.to_string()))
}

/// Shows a system notification through the desktop shell. Failures are logged, never shown.
pub async fn notify(title: &str, maybe_body: Option<&str>) {
  show_notification(None, title, maybe_body).await;
}

/// Shows the notification of a conversation; a newer one replaces it, and
/// [`withdraw_notifications`] removes it once the conversation is read.
pub async fn notify_conversation(conversation: ConversationId, title: &str, maybe_body: Option<&str>) {
  show_notification(Some(&conversation.to_string()), title, maybe_body).await;
}

/// Removes the notifications of conversations that were read (here or on another device): the page's
/// own, the service worker's push notifications (tagged with the conversation ID too) and the desktop
/// shell's, where the platform allows.
pub async fn withdraw_notifications(conversations: &[ConversationId]) {
  if conversations.is_empty() {
    return;
  }
  let tags: Vec<String> = conversations.iter().map(ToString::to_string).collect();
  if is_available() {
    let args = serde_json::json!({ "tags": tags });
    if let Err(error) = call::<()>("withdraw_notifications", &args).await {
      tracing::debug!(%error, "could not withdraw notifications");
    }
    return;
  }
  close_page_notifications(&tags);
  close_service_worker_notifications(&tags).await;
}

async fn show_notification(maybe_tag: Option<&str>, title: &str, maybe_body: Option<&str>) {
  if !is_available() {
    show_page_notification(maybe_tag, title, maybe_body);
    return;
  }
  let args = serde_json::json!({ "title": title, "body": maybe_body, "tag": maybe_tag });
  if let Err(error) = call::<()>("notify_user", &args).await {
    tracing::debug!(%error, "could not show a notification");
  }
}

/// What the desktop shell knows about the computer: input anywhere and the screen lock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct OsAttention {
  #[serde(rename = "idle_ms", default)]
  pub maybe_idle_ms: Option<u64>,
  #[serde(default)]
  pub is_locked: bool,
}

/// Desktop only: OS idle time and screen lock. `None` in the browser or when the shell can't tell.
pub async fn os_attention() -> Option<OsAttention> {
  if !is_available() {
    return None;
  }
  call::<OsAttention>("os_attention", &serde_json::json!({}))
    .await
    .inspect_err(|error| tracing::debug!(%error, "no OS attention"))
    .ok()
}

/// Calls a desktop command with JSON arguments and reads its JSON answer. Errors come back as text.
pub async fn call<T: for<'de> Deserialize<'de>>(
  command: &str,
  args: &serde_json::Value,
) -> Result<T, String> {
  if !is_available() {
    return call_web_core(command, args).await;
  }
  let js_args =
    js_sys::JSON::parse(&args.to_string()).map_err(|_| "could not encode the arguments".to_owned())?;
  let answer = invoke(command, js_args).await.map_err(|error| match error {
    AuthError::InvalidResponse(message) | AuthError::Denied(message) | AuthError::Network(message) => message,
    other => other.to_string(),
  })?;
  // Commands without a result answer with `null`/undefined; `T` is then `()` or an `Option`.
  let text = js_sys::JSON::stringify(&answer)
    .ok()
    .and_then(|text| text.as_string())
    .unwrap_or_else(|| "null".to_owned());
  serde_json::from_str(&text).map_err(|error| format!("unreadable answer of {command}: {error}"))
}

/// The browser has no shell: the core commands are answered by the core running in the page.
#[cfg(target_arch = "wasm32")]
async fn call_web_core<T: for<'de> Deserialize<'de>>(
  command: &str,
  args: &serde_json::Value,
) -> Result<T, String> {
  let answer = crate::web_core::call(command, args.clone()).await?;
  serde_json::from_value(answer).map_err(|error| format!("unreadable answer of {command}: {error}"))
}

#[cfg(not(target_arch = "wasm32"))]
async fn call_web_core<T: for<'de> Deserialize<'de>>(
  command: &str,
  _args: &serde_json::Value,
) -> Result<T, String> {
  Err(format!("{command} needs the desktop app or a browser"))
}

/// Whether the client core is reachable: in the desktop shell, or in the page (browser build).
pub fn has_core() -> bool {
  #[cfg(target_arch = "wasm32")]
  {
    is_available() || crate::web_core::is_available()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    is_available()
  }
}

/// Calls `callback` whenever the desktop shell emits `event`.
pub fn listen(event: &str, callback: impl FnMut() + 'static) {
  if !is_available() {
    #[cfg(target_arch = "wasm32")]
    crate::web_core::listen(callback);
    #[cfg(not(target_arch = "wasm32"))]
    drop((event, callback));
    return;
  }
  let Some(internals) = internals() else {
    tracing::warn!(event, "the desktop event bridge is not available");
    return;
  };
  let transform = js_sys::Reflect::get(&internals, &JsValue::from_str("transformCallback"))
    .ok()
    .and_then(|value| value.dyn_into::<js_sys::Function>().ok());
  let (Some(invoke), Some(transform)) = (invoke_function(), transform) else {
    tracing::warn!(event, "the desktop event bridge is not available");
    return;
  };
  let mut callback = callback;
  let handler =
    wasm_bindgen::closure::Closure::<dyn FnMut(JsValue)>::new(move |_payload: JsValue| callback());
  // Same call the Tauri JS API makes: register the callback, then subscribe to the event with it.
  let Ok(callback_id) = transform.call1(&internals, handler.as_ref()) else {
    tracing::warn!(event, "could not register the event callback");
    return;
  };
  let args = js_sys::Object::new();
  let target = js_sys::Object::new();
  let set = |object: &js_sys::Object, key: &str, value: &JsValue| {
    drop(js_sys::Reflect::set(object, &JsValue::from_str(key), value));
  };
  set(&target, "kind", &JsValue::from_str("Any"));
  set(&args, "event", &JsValue::from_str(event));
  set(&args, "target", &target);
  set(&args, "handler", &callback_id);
  if invoke
    .call2(&JsValue::NULL, &JsValue::from_str("plugin:event|listen"), &args)
    .is_err()
  {
    tracing::warn!(event, "could not subscribe to the desktop event");
  }
  // The subscription lives as long as the page.
  handler.forget();
}

/// `window.__TAURI_INTERNALS__`: what the shell injects into its webview for the command bridge.
/// The shell does not set `withGlobalTauri`, so scripts find no friendlier `window.__TAURI__`.
fn internals() -> Option<JsValue> {
  let window = web_sys::window()?;
  let internals = js_sys::Reflect::get(&window, &JsValue::from_str("__TAURI_INTERNALS__")).ok()?;
  (!internals.is_undefined() && !internals.is_null()).then_some(internals)
}

fn invoke_function() -> Option<js_sys::Function> {
  js_sys::Reflect::get(&internals()?, &JsValue::from_str("invoke"))
    .ok()?
    .dyn_into()
    .ok()
}

/// The browser's `Notification` object, if it has one.
fn notification_constructor() -> Option<js_sys::Function> {
  let window = web_sys::window()?;
  js_sys::Reflect::get(&window, &JsValue::from_str("Notification"))
    .ok()?
    .dyn_into()
    .ok()
}

/// `granted`, `denied` or `default` (not asked yet); `None` where the browser has no notifications.
pub fn notification_permission() -> Option<String> {
  let constructor = notification_constructor()?;
  js_sys::Reflect::get(&constructor, &JsValue::from_str("permission"))
    .ok()?
    .as_string()
}

/// Asks the user to allow notifications (must run from a click). Returns the new permission.
pub async fn request_notification_permission() -> Option<String> {
  let constructor = notification_constructor()?;
  let request: js_sys::Function = js_sys::Reflect::get(&constructor, &JsValue::from_str("requestPermission"))
    .ok()?
    .dyn_into()
    .ok()?;
  let promise: js_sys::Promise = request.call0(&constructor).ok()?.dyn_into().ok()?;
  JsFuture::from(promise).await.ok()?.as_string()
}

thread_local! {
  /// The page's own notifications by conversation, so they can be closed once read.
  static PAGE_NOTIFICATIONS: RefCell<HashMap<String, JsValue>> = RefCell::new(HashMap::new());
}

/// Shows a notification from the page while it is open (closed pages get Web Push through the
/// service worker); without permission nothing is shown.
fn show_page_notification(maybe_tag: Option<&str>, title: &str, maybe_body: Option<&str>) {
  if notification_permission().as_deref() != Some("granted") {
    return;
  }
  let Some(constructor) = notification_constructor() else {
    return;
  };
  let options = js_sys::Object::new();
  if let Some(body) = maybe_body {
    drop(js_sys::Reflect::set(&options, &"body".into(), &body.into()));
  }
  if let Some(tag) = maybe_tag {
    drop(js_sys::Reflect::set(&options, &"tag".into(), &tag.into()));
  }
  let args = js_sys::Array::of2(&JsValue::from_str(title), &options);
  match js_sys::Reflect::construct(&constructor, &args) {
    Ok(notification) => {
      if let Some(tag) = maybe_tag {
        PAGE_NOTIFICATIONS.with(|shown| shown.borrow_mut().insert(tag.to_owned(), notification));
      }
    },
    Err(_) => tracing::debug!("could not show a notification"),
  }
}

fn close_page_notifications(tags: &[String]) {
  let closing: Vec<JsValue> = PAGE_NOTIFICATIONS.with(|shown| {
    let mut shown = shown.borrow_mut();
    tags.iter().filter_map(|tag| shown.remove(tag)).collect()
  });
  for notification in closing {
    close_notification(&notification);
  }
}

/// Push notifications the service worker showed while no page was open carry the conversation ID
/// as their tag (`teamcraft_core::web_push`).
async fn close_service_worker_notifications(tags: &[String]) {
  let Some(window) = web_sys::window() else { return };
  let Ok(ready) = window.navigator().service_worker().ready() else {
    return;
  };
  let Ok(registration) = JsFuture::from(ready).await else {
    return;
  };
  let get_notifications = js_sys::Reflect::get(&registration, &"getNotifications".into())
    .ok()
    .and_then(|value| value.dyn_into::<js_sys::Function>().ok());
  let Some(get_notifications) = get_notifications else {
    return;
  };
  let Ok(promise) = get_notifications.call0(&registration) else {
    return;
  };
  let Ok(shown) = JsFuture::from(js_sys::Promise::from(promise)).await else {
    return;
  };
  for notification in js_sys::Array::from(&shown).iter() {
    let tag = js_sys::Reflect::get(&notification, &"tag".into())
      .ok()
      .and_then(|value| value.as_string());
    if tag.is_some_and(|tag| tags.contains(&tag)) {
      close_notification(&notification);
    }
  }
}

fn close_notification(notification: &JsValue) {
  let close = js_sys::Reflect::get(notification, &"close".into())
    .ok()
    .and_then(|value| value.dyn_into::<js_sys::Function>().ok());
  if let Some(close) = close
    && close.call0(notification).is_err()
  {
    tracing::debug!("could not close a notification");
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn command_error_kinds_map_to_auth_errors() {
    assert_eq!(map_kind("cancelled", String::new()), AuthError::Cancelled);
    assert_eq!(
      map_kind("network", "down".into()),
      AuthError::Network("down".into())
    );
    assert_eq!(map_kind("not_signed_in", String::new()), AuthError::NotSignedIn);
    assert!(matches!(
      map_kind("something_new", "x".into()),
      AuthError::InvalidResponse(_)
    ));
  }
}
