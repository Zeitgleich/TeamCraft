//! Desktop sign-in: the Tauri shell (Rust side) runs the OIDC flow with the system browser and keeps
//! the tokens in the OS keyring; the webview only calls commands and never sees a token.

use serde::Deserialize;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use super::{ApiMethod, ApiResponse, AuthError, Authenticator, LocalFuture};

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
  let args = serde_json::json!({ "title": title, "body": maybe_body });
  if let Err(error) = call::<()>("notify_user", &args).await {
    tracing::debug!(%error, "could not show a notification");
  }
}

/// Calls a desktop command with JSON arguments and reads its JSON answer. Errors come back as text.
pub async fn call<T: for<'de> Deserialize<'de>>(
  command: &str,
  args: &serde_json::Value,
) -> Result<T, String> {
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

/// Calls `callback` whenever the desktop shell emits `event`.
pub fn listen(event: &str, callback: impl FnMut() + 'static) {
  let listener = (|| -> Option<(JsValue, js_sys::Function)> {
    let window = web_sys::window()?;
    let tauri = js_sys::Reflect::get(&window, &JsValue::from_str("__TAURI__")).ok()?;
    let events = js_sys::Reflect::get(&tauri, &JsValue::from_str("event")).ok()?;
    let listen = js_sys::Reflect::get(&events, &JsValue::from_str("listen"))
      .ok()?
      .dyn_into::<js_sys::Function>()
      .ok()?;
    Some((events, listen))
  })();
  let Some((events, function)) = listener else {
    tracing::warn!(event, "the desktop event bridge is not available");
    return;
  };
  let mut callback = callback;
  let handler =
    wasm_bindgen::closure::Closure::<dyn FnMut(JsValue)>::new(move |_payload: JsValue| callback());
  if function
    .call2(&events, &JsValue::from_str(event), handler.as_ref())
    .is_err()
  {
    tracing::warn!(event, "could not subscribe to the desktop event");
  }
  // The subscription lives as long as the page.
  handler.forget();
}

/// `window.__TAURI__.core.invoke`, present when the shell sets `withGlobalTauri`.
fn invoke_function() -> Option<js_sys::Function> {
  let window = web_sys::window()?;
  let tauri = js_sys::Reflect::get(&window, &JsValue::from_str("__TAURI__")).ok()?;
  let core = js_sys::Reflect::get(&tauri, &JsValue::from_str("core")).ok()?;
  js_sys::Reflect::get(&core, &JsValue::from_str("invoke"))
    .ok()?
    .dyn_into()
    .ok()
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
