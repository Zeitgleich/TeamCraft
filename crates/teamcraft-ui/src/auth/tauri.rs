//! Desktop sign-in: the Tauri shell (Rust side) runs the OIDC flow with the system browser and keeps
//! the tokens in the OS keyring; the webview only calls commands and never sees a token.

use std::cell::RefCell;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use teamcraft_client_api::actions::FileInfo;
use teamcraft_client_api::video_playback::VideoPlayback;
use teamcraft_client_api::window_chrome::{TitleBar, TrayIconStyle, WindowAction, WindowChrome};
use teamcraft_proto::{AvatarContentType, UserId};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use super::{
  ApiBytes, ApiMethod, ApiResponse, AuthError, Authenticator, BinaryBody, LocalFuture, SignInLook,
};
use teamcraft_client_api::call_window::CallWindow;
use teamcraft_client_api::os_notify::{NotificationSound, NotificationTag, OsNotification, TagTarget};

use crate::clock::now_ms;
use crate::feedback::{ActionFailure, FailureKind, NotificationFailure};
use crate::model::{ConversationId, TimestampMs};
use crate::reactive::OwnerLiveness;

/// Prefix of the i18n keys the server commands answer with.
const SERVER_KEY_PREFIX: &str = "server-entry-";
/// The message of a server step that failed for another reason.
const SERVER_FAILED_KEY: &str = "server-entry-failed";

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

  fn login(&self, look: SignInLook) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async move {
      let args = js_sys::JSON::parse(&serde_json::json!({ "look": look }).to_string())
        .map_err(|_| AuthError::InvalidResponse("could not build the request".to_owned()))?;
      invoke("auth_login", args).await.map(|_| ())
    })
  }

  fn login_local(
    &self,
    login: String,
    password: String,
    maybe_totp: Option<String>,
  ) -> LocalFuture<'_, Result<(), AuthError>> {
    Box::pin(async move {
      let invalid = |_| AuthError::InvalidResponse("could not build the request".to_owned());
      let args = js_sys::Object::new();
      js_sys::Reflect::set(&args, &"login".into(), &login.into()).map_err(invalid)?;
      js_sys::Reflect::set(&args, &"password".into(), &password.into()).map_err(invalid)?;
      if let Some(code) = maybe_totp {
        js_sys::Reflect::set(&args, &"totp_code".into(), &code.into()).map_err(invalid)?;
      }
      invoke("auth_login_local", args.into()).await.map(|_| ())
    })
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

  fn api_request_bytes(
    &self,
    method: ApiMethod,
    path: &str,
    maybe_body: Option<BinaryBody>,
  ) -> LocalFuture<'_, Result<ApiBytes, AuthError>> {
    use base64::Engine;

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
        let encoded = base64::engine::general_purpose::STANDARD.encode(&body.bytes);
        js_sys::Reflect::set(&args, &"content_type".into(), &body.content_type.into()).map_err(invalid)?;
        js_sys::Reflect::set(&args, &"body_base64".into(), &encoded.into()).map_err(invalid)?;
      }
      let value = invoke("api_request_bytes", args.into()).await?;
      let response: ApiBytesPayload = serde_wasm_json(&value)?;
      let body = base64::engine::general_purpose::STANDARD
        .decode(response.body_base64)
        .map_err(|_| AuthError::InvalidResponse("the body is not base64".to_owned()))?;
      Ok(ApiBytes {
        status: response.status,
        maybe_content_type: response.content_type,
        body,
      })
    })
  }
}

/// The picture of `user`, downloaded by the shell from the presigned URL the API names: the
/// webview's content security policy keeps object storage out of the page.
pub async fn avatar_download(user: UserId) -> Result<ApiBytes, AuthError> {
  use base64::Engine;

  let invalid = |_| AuthError::InvalidResponse("could not build the request".to_owned());
  let args = js_sys::Object::new();
  js_sys::Reflect::set(&args, &"user_id".into(), &user.to_string().into()).map_err(invalid)?;
  let value = invoke("avatar_download", args.into()).await?;
  let response: ApiBytesPayload = serde_wasm_json(&value)?;
  let body = base64::engine::general_purpose::STANDARD
    .decode(response.body_base64)
    .map_err(|_| AuthError::InvalidResponse("the body is not base64".to_owned()))?;
  Ok(ApiBytes {
    status: response.status,
    maybe_content_type: response.content_type,
    body,
  })
}

/// Makes `bytes` the signed-in user's picture through the shell (announce, upload, complete). The
/// answer is the first refusal or the completion's, like an API call's.
pub async fn avatar_upload(content_type: &AvatarContentType, bytes: &[u8]) -> Result<ApiResponse, AuthError> {
  use base64::Engine;

  let invalid = |_| AuthError::InvalidResponse("could not build the request".to_owned());
  let args = js_sys::Object::new();
  let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
  js_sys::Reflect::set(&args, &"content_type".into(), &content_type.as_str().into()).map_err(invalid)?;
  js_sys::Reflect::set(&args, &"body_base64".into(), &encoded.into()).map_err(invalid)?;
  let value = invoke("avatar_upload", args.into()).await?;
  let response: ApiPayload = serde_wasm_json(&value)?;
  Ok(ApiResponse {
    status: response.status,
    body: response.body,
  })
}

#[derive(Deserialize)]
struct ApiBytesPayload {
  status: u16,
  content_type: Option<String>,
  body_base64: String,
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

/// Calls a desktop command with a raw body (no JSON) and string headers; for bulk bytes such as the
/// chunks of a staged file. The answer is the command's JSON result.
pub(crate) async fn invoke_raw(
  command: &str,
  body: &js_sys::Uint8Array,
  headers: &[(&str, String)],
) -> Result<JsValue, String> {
  let function = invoke_function().ok_or_else(|| "not running in the desktop app".to_owned())?;
  let header_map = js_sys::Object::new();
  for (name, value) in headers {
    js_sys::Reflect::set(&header_map, &JsValue::from_str(name), &JsValue::from_str(value))
      .map_err(|_| "could not encode the headers".to_owned())?;
  }
  let options = js_sys::Object::new();
  js_sys::Reflect::set(&options, &JsValue::from_str("headers"), &header_map)
    .map_err(|_| "could not encode the headers".to_owned())?;
  let promise = function
    .call3(&JsValue::NULL, &command.into(), body, &options)
    .map_err(|_| "the desktop bridge rejected the call".to_owned())?;
  let promise: js_sys::Promise = promise.dyn_into().map_err(|_| "no promise".to_owned())?;
  JsFuture::from(promise)
    .await
    .map_err(|error| error_from_js(error).to_string())
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
    // Refusals of the local sign-in keep their kind for the form's message.
    "refused" | "disabled" | "rate_limited" | "mfa_required" | "account_deactivated" => {
      AuthError::Denied(kind.to_owned())
    },
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

/// The account's server as the desktop shell reports it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ServerView {
  pub origin: String,
  pub host: String,
  pub display_name: String,
  #[serde(default)]
  pub has_local_accounts: bool,
}

/// Where the server entry stands at startup.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ServerState {
  #[serde(rename = "active", default)]
  pub maybe_active: Option<ServerView>,
  #[serde(rename = "managed_address", default)]
  pub maybe_managed_address: Option<String>,
  #[serde(default)]
  pub is_locked: bool,
}

/// A server the shell checked, waiting for the user's confirmation.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ServerCheck {
  pub server: ServerView,
  pub brand: teamcraft_proto::BrandResponse,
  /// Where the password will be entered.
  #[serde(default)]
  pub identity_provider_host: String,
  /// The identity provider lies outside the server's domain.
  #[serde(default)]
  pub is_identity_provider_elsewhere: bool,
  /// The brand image as a `data:` URL (an older shell sends none).
  #[serde(rename = "logo", default)]
  pub maybe_logo: Option<String>,
  /// The brand's separate image for the dark theme.
  #[serde(rename = "logo_dark", default)]
  pub maybe_logo_dark: Option<String>,
}

/// A failed server step: the i18n key of the message (`server-entry-*`) and details for the
/// collapsed "technical details".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerProblem {
  pub key: String,
  pub detail: String,
}

/// The i18n key of a server address that does not resolve; the entry then offers to look for a
/// similar one ([`server_suggest`]).
pub const UNKNOWN_HOST_KEY: &str = "server-entry-unknown-host";

/// The server entry's state in the shell.
pub async fn server_state() -> Result<ServerState, ServerProblem> {
  server_call("server_state", &serde_json::json!({})).await
}

/// Asks the server at `address` (as typed) how to sign in; the shell keeps the answer for
/// [`server_confirm`].
pub async fn server_check(address: &str) -> Result<ServerCheck, ServerProblem> {
  server_call("server_check", &serde_json::json!({ "address": address })).await
}

/// "Did you mean …?" for an address that does not resolve: the shell looks up the same name under
/// other common top-level domains. Only on the user's request; the answer is only suggested.
pub async fn server_suggest(address: &str) -> Result<Option<String>, ServerProblem> {
  server_call("server_suggest", &serde_json::json!({ "address": address })).await
}

/// Makes the checked server the account's server.
pub async fn server_confirm(origin: &str) -> Result<ServerView, ServerProblem> {
  server_call("server_confirm", &serde_json::json!({ "origin": origin })).await
}

/// Leaves the account's server (after sign-out): the server entry comes next.
pub async fn server_forget() -> Result<(), ServerProblem> {
  server_call("server_forget", &serde_json::json!({})).await
}

/// Like [`call`], keeping the error's kind, which the server commands set to an i18n key.
async fn server_call<T: for<'de> Deserialize<'de>>(
  command: &str,
  args: &serde_json::Value,
) -> Result<T, ServerProblem> {
  let failed = |detail: String| ServerProblem {
    key: SERVER_FAILED_KEY.to_owned(),
    detail,
  };
  let function = invoke_function().ok_or_else(|| failed("not running in the desktop app".to_owned()))?;
  let js_args = js_sys::JSON::parse(&args.to_string())
    .map_err(|_| failed("could not encode the arguments".to_owned()))?;
  let promise = function
    .call2(&JsValue::NULL, &command.into(), &js_args)
    .map_err(|_| failed("the desktop bridge rejected the call".to_owned()))?
    .dyn_into::<js_sys::Promise>()
    .map_err(|_| failed("no promise".to_owned()))?;
  let answer =
    JsFuture::from(promise)
      .await
      .map_err(|value| match serde_wasm_json::<CommandError>(&value) {
        Ok(error) if error.kind.starts_with(SERVER_KEY_PREFIX) => ServerProblem {
          key: error.kind,
          detail: error.message,
        },
        Ok(error) => failed(error.message),
        Err(_) => failed(value.as_string().unwrap_or_default()),
      })?;
  serde_wasm_json(&answer).map_err(|error| failed(error.to_string()))
}

/// Shows a system notification: through the desktop shell, or from the page (the service worker's
/// registration where there is one, so buttons work and a click reaches the worker). A newer one with
/// the same tag replaces it. Failures are logged, never shown.
pub async fn show_notification(notification: &OsNotification) {
  if let Err(failure) = try_show_notification(notification).await {
    tracing::debug!(detail = %failure.detail, "could not show a notification");
  }
}

/// [`show_notification`] for a caller that tells the user what happened (the test notification in
/// the settings): why it could not be shown. The system may still hide a shown one (focus modes).
pub async fn try_show_notification(notification: &OsNotification) -> Result<(), ActionFailure> {
  if !is_available() {
    return show_page_notification(notification).await;
  }
  let args = serde_json::json!({ "notification": notification });
  call::<()>("notify_user", &args)
    .await
    .map_err(|error| ActionFailure::from_wire(&error))
}

/// Removes the notifications of conversations that were read (here or on another device).
pub async fn withdraw_notifications(conversations: &[ConversationId]) {
  let tags: Vec<NotificationTag> = conversations.iter().map(NotificationTag::conversation).collect();
  withdraw_tags(&tags).await;
}

/// Removes notifications by tag: the page's own, the service worker's push notifications (tagged with
/// the conversation ID too) and the desktop shell's.
pub async fn withdraw_tags(tags: &[NotificationTag]) {
  if tags.is_empty() {
    return;
  }
  let tags: Vec<String> = tags.iter().map(|tag| tag.as_str().to_owned()).collect();
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

/// Starts or stops the ringtone of an incoming call: in the desktop shell, or in the page.
pub async fn set_ringtone(is_ringing: bool) {
  if !is_available() {
    // The browser rings in the page, in the tab that runs the calls.
    #[cfg(target_arch = "wasm32")]
    crate::web_calls::set_ringing(is_ringing);
    return;
  }
  let args = serde_json::json!({ "is_ringing": is_ringing });
  if let Err(error) = call::<()>("set_ringtone", &args).await {
    tracing::debug!(%error, "could not switch the ringtone");
  }
}

/// Desktop: shows the incoming-call window (#650); `false` if the shell couldn't or the window
/// can't be relied on there (Wayland, #962), so the caller shows the notification instead.
pub async fn show_call_window(window: &CallWindow) -> bool {
  if !is_available() {
    return false;
  }
  let args = serde_json::json!({ "window": window });
  match call::<bool>("call_window_show", &args).await {
    Ok(is_shown) => is_shown,
    Err(error) => {
      tracing::warn!(%error, "could not show the incoming-call window");
      false
    },
  }
}

/// Desktop: closes the incoming-call window if it shows the call with this tag.
pub async fn close_call_window(tag: &NotificationTag) {
  if !is_available() {
    return;
  }
  let args = serde_json::json!({ "tag": tag });
  if let Err(error) = call::<()>("call_window_close", &args).await {
    tracing::debug!(%error, "could not close the incoming-call window");
  }
}

/// The label of the shell's window this page runs in; `None` in a browser.
pub fn window_label() -> Option<String> {
  let metadata = js_sys::Reflect::get(&internals()?, &JsValue::from_str("metadata")).ok()?;
  let current = js_sys::Reflect::get(&metadata, &JsValue::from_str("currentWindow")).ok()?;
  js_sys::Reflect::get(&current, &JsValue::from_str("label"))
    .ok()?
    .as_string()
}

/// Desktop: asks for the user's attention (taskbar flash, dock bounce) or stops asking.
pub async fn request_attention(is_wanted: bool) {
  if !is_available() {
    return;
  }
  let args = serde_json::json!({ "is_wanted": is_wanted });
  if let Err(error) = call::<()>("request_attention", &args).await {
    tracing::debug!(%error, "could not ask for attention");
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

/// Desktop only: unread count (`None` keeps the badge) and menu texts for the tray icon and the dock/taskbar badge.
/// `maybe_menu` are the macOS menu bar's texts (the shell's `AppMenuLabels`); `None` leaves them.
pub async fn tray_update(
  maybe_unread: Option<u32>,
  tooltip: &str,
  show_label: &str,
  quit_label: &str,
  maybe_menu: Option<serde_json::Value>,
) {
  if !is_available() {
    return;
  }
  let args = serde_json::json!({
    "update": {
      "maybe_unread": maybe_unread,
      "tooltip": tooltip,
      "show_label": show_label,
      "quit_label": quit_label,
      "menu": maybe_menu,
    }
  });
  if let Err(error) = call::<()>("tray_update", &args).await {
    tracing::debug!(%error, "could not update the tray");
  }
}

/// Desktop only: zooms the webview (percent, 100 = normal).
pub async fn set_zoom(percent: u16) {
  if !is_available() {
    return;
  }
  let args = serde_json::json!({ "factor": f64::from(percent) / 100.0 });
  if let Err(error) = call::<()>("set_zoom", &args).await {
    tracing::debug!(%error, "could not zoom");
  }
}

/// The desktop shell's own preferences (closing to the tray, starting minimized, the title bar,
/// the tray icon's look). Sent back whole, so every field the shell has must be here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellPrefs {
  pub is_close_to_tray: bool,
  pub is_start_minimized: bool,
  #[serde(default)]
  pub title_bar: TitleBar,
  #[serde(default)]
  pub tray_icon: TrayIconStyle,
}

/// Desktop only: who draws the title bar and which window controls the UI shows.
pub async fn window_chrome() -> Option<WindowChrome> {
  if !is_available() {
    return None;
  }
  call::<WindowChrome>("window_chrome", &serde_json::json!({}))
    .await
    .inspect_err(|error| tracing::debug!(%error, "no window chrome"))
    .ok()
}

/// Desktop only: minimize, maximize, close or move the window from the custom title bar.
pub async fn window_action(action: WindowAction) {
  if let Err(error) = call::<()>("window_action", &serde_json::json!({ "action": action })).await {
    tracing::debug!(%error, ?action, "window action failed");
  }
}

/// `None` outside the desktop app.
pub async fn shell_prefs() -> Option<ShellPrefs> {
  if !is_available() {
    return None;
  }
  call::<ShellPrefs>("shell_prefs_get", &serde_json::json!({}))
    .await
    .inspect_err(|error| tracing::debug!(%error, "no shell preferences"))
    .ok()
}

pub async fn set_shell_prefs(prefs: ShellPrefs) -> Result<(), String> {
  call::<()>("shell_prefs_set", &serde_json::json!({ "prefs": prefs })).await
}

/// "Start at login": `Ok(None)` outside the desktop app or where the system can't do it (the
/// switch is hidden then), an error when the state couldn't be read.
pub async fn autostart() -> Result<Option<bool>, String> {
  if !is_available() {
    return Ok(None);
  }
  call::<Option<bool>>("autostart_get", &serde_json::json!({})).await
}

/// Turns "start at login" on or off; answers the state the system reports afterwards.
pub async fn set_autostart(is_enabled: bool) -> Result<bool, String> {
  call::<bool>("autostart_set", &serde_json::json!({ "enabled": is_enabled })).await
}

/// Desktop only: where the update machinery is (docs/updates.md §4). `None` in the browser.
pub async fn update_status() -> Option<teamcraft_client_api::updates::UpdateStatus> {
  if !is_available() {
    return None;
  }
  call("update_status", &serde_json::json!({}))
    .await
    .inspect_err(|error| tracing::debug!(%error, "no update status"))
    .ok()
}

/// Desktop only: "Check now".
pub async fn update_check() -> Result<(), String> {
  call::<()>("update_check", &serde_json::json!({})).await
}

pub async fn update_set_preference(
  preference: teamcraft_client_api::updates::UpdatePreference,
) -> Result<(), String> {
  call::<()>(
    "update_set_preference",
    &serde_json::json!({ "preference": preference }),
  )
  .await
}

/// Notify-only: downloads the update now.
pub async fn update_download() -> Result<(), String> {
  call::<()>("update_download", &serde_json::json!({})).await
}

/// Installs the downloaded update and restarts; refused during a call.
pub async fn update_restart() -> Result<(), String> {
  call::<()>("update_restart", &serde_json::json!({})).await
}

/// deb/rpm: asks PackageKit to install the update.
pub async fn update_with_package_manager() -> Result<(), String> {
  call::<()>("update_with_package_manager", &serde_json::json!({})).await
}

/// Hides the hint for this version for good (deb, rpm, Flatpak: the app can't install it).
pub async fn update_dismiss(version: &str) -> Result<(), String> {
  call::<()>("update_dismiss", &serde_json::json!({ "version": version })).await
}

/// Linux desktop only: which parts for phone videos GStreamer has (#804). `None` in the browser and
/// on systems whose webview doesn't play through GStreamer.
pub async fn video_playback() -> Option<VideoPlayback> {
  if !is_available() {
    return None;
  }
  call::<Option<VideoPlayback>>("video_playback", &serde_json::json!({}))
    .await
    .inspect_err(|error| tracing::debug!(%error, "no video playback report"))
    .ok()
    .flatten()
}

/// Desktop only: opens a video attachment in the system's default player.
pub async fn open_video_externally(file: &FileInfo) -> Result<(), String> {
  let args = serde_json::json!({
    "conversation": file.conversation,
    "message": file.message,
    "file_id": file.file_id,
    "file_name": file.file_name,
    "media_type": file.media_type,
  });
  call::<()>("open_video_externally", &args).await
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

/// Calls `callback` whenever the desktop shell emits `event`, while the current reactive owner lives
/// (see [`OwnerLiveness`]).
pub fn listen(event: &str, callback: impl FnMut() + 'static) {
  let liveness = OwnerLiveness::of_current_owner();
  let mut callback = callback;
  let callback = move || {
    if liveness.is_alive() {
      callback();
    }
  };
  if !is_available() {
    #[cfg(target_arch = "wasm32")]
    crate::web_core::listen(callback);
    #[cfg(not(target_arch = "wasm32"))]
    drop((event, callback));
    return;
  }
  let mut callback = callback;
  subscribe(event, move |_| callback());
}

/// Calls `callback` with the text payload of every `event` the desktop shell emits (desktop only),
/// while the current reactive owner lives (see [`OwnerLiveness`]).
pub fn listen_text(event: &str, mut callback: impl FnMut(String) + 'static) {
  if !is_available() {
    return;
  }
  let liveness = OwnerLiveness::of_current_owner();
  subscribe(event, move |message| {
    if !liveness.is_alive() {
      return;
    }
    // Tauri delivers `{ event, id, payload }`.
    if let Some(text) = js_sys::Reflect::get(&message, &JsValue::from_str("payload"))
      .ok()
      .and_then(|payload| payload.as_string())
    {
      callback(text);
    }
  });
}

fn subscribe(event: &str, callback: impl FnMut(JsValue) + 'static) {
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
  let handler = wasm_bindgen::closure::Closure::<dyn FnMut(JsValue)>::new(callback);
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
  // The subscription lives as long as the page; `listen` and `listen_text` make it go quiet with
  // its owner.
  handler.forget();
}

/// Registers `callback` with the command bridge and returns the argument that hands it to a
/// command taking a `tauri::ipc::Channel` (what `new Channel()` of the Tauri JS API does). The
/// callback gets `{ message, index }` per message (in send order by `index`, but not necessarily
/// delivered in it) and `{ end: true, index }` when the shell drops the channel.
#[cfg(target_arch = "wasm32")]
pub(crate) fn open_channel(callback: &js_sys::Function) -> Option<(u32, String)> {
  let internals = internals()?;
  let transform: js_sys::Function = js_sys::Reflect::get(&internals, &JsValue::from_str("transformCallback"))
    .ok()?
    .dyn_into()
    .ok()?;
  let id = transform
    .call2(&internals, callback, &JsValue::FALSE)
    .ok()?
    .as_f64()?;
  // The bridge's ids are random `u32`s, so the cast is exact.
  let id = id as u32;
  Some((id, format!("__CHANNEL__:{id}")))
}

/// Forgets a callback of [`open_channel`]; later messages of its channel are dropped.
#[cfg(target_arch = "wasm32")]
pub(crate) fn close_channel(id: u32) {
  let Some(internals) = internals() else {
    return;
  };
  if let Some(unregister) = js_sys::Reflect::get(&internals, &JsValue::from_str("unregisterCallback"))
    .ok()
    .and_then(|value| value.dyn_into::<js_sys::Function>().ok())
  {
    // Nothing to do when it fails: the callback then just stays registered.
    let _ = unregister.call1(&internals, &JsValue::from(id));
  }
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

/// A `Notification.requestPermission()` that is running. Browsers show their prompt only when the
/// request is made inside the click's own task (Safari and Chrome check the user gesture there and
/// otherwise refuse without a prompt, #893), so [`request_notification_permission`] starts it
/// synchronously and [`Self::answer`] waits for the user's reply afterwards.
pub struct NotificationPermissionRequest {
  maybe_promise: Option<js_sys::Promise>,
  /// When the request was made; `None` where nothing was asked.
  maybe_asked_at_ms: Option<TimestampMs>,
}

/// The browser's reply to a [`NotificationPermissionRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationPermissionAnswer {
  /// `granted`, `denied`, or `default` when no prompt was shown. `None` where the browser has no
  /// notifications.
  pub maybe_permission: Option<String>,
  /// How long the browser took to reply, where it was asked. A `denied` in a few milliseconds came
  /// without a prompt (a private window, #893); a person needs longer to read one.
  pub maybe_reply_after_ms: Option<i64>,
}

/// Starts the browser's permission prompt. Call it first thing in the click handler, before any
/// `.await` or `spawn_local`. Nothing is asked where the permission is granted or denied already.
pub fn request_notification_permission() -> NotificationPermissionRequest {
  let maybe_promise = (notification_permission().as_deref() == Some("default"))
    .then(start_permission_request)
    .flatten();
  let maybe_asked_at_ms = maybe_promise.as_ref().map(|_| now_ms());
  NotificationPermissionRequest {
    maybe_promise,
    maybe_asked_at_ms,
  }
}

impl NotificationPermissionRequest {
  /// The permission once the user answered (or the browser gave none), and how long that took.
  pub async fn answer(self) -> NotificationPermissionAnswer {
    if let Some(promise) = self.maybe_promise {
      // A rejected request is the same as no answer; the permission below says what is the case.
      let _ = JsFuture::from(promise).await;
    }
    NotificationPermissionAnswer {
      maybe_permission: notification_permission(),
      maybe_reply_after_ms: self.maybe_asked_at_ms.map(|asked_at| now_ms() - asked_at),
    }
  }
}

/// Whether this browser can receive Web Push (a service worker and the Push API). Without it, as in
/// some private windows, turning notifications on can't work, so the app doesn't offer it.
pub fn has_web_push() -> bool {
  let Some(window) = web_sys::window() else {
    return false;
  };
  let has_push_manager = js_sys::Reflect::has(&window, &JsValue::from_str("PushManager")).unwrap_or(false);
  let has_service_worker = js_sys::Reflect::get(&window, &JsValue::from_str("navigator"))
    .and_then(|navigator| js_sys::Reflect::get(&navigator, &JsValue::from_str("serviceWorker")))
    .is_ok_and(|worker| !worker.is_undefined() && !worker.is_null());
  has_push_manager && has_service_worker
}

fn start_permission_request() -> Option<js_sys::Promise> {
  let constructor = notification_constructor()?;
  let request: js_sys::Function = js_sys::Reflect::get(&constructor, &JsValue::from_str("requestPermission"))
    .ok()?
    .dyn_into()
    .ok()?;
  request.call0(&constructor).ok()?.dyn_into().ok()
}

thread_local! {
  /// The page's own notifications by conversation, so they can be closed once read.
  static PAGE_NOTIFICATIONS: RefCell<HashMap<String, JsValue>> = RefCell::new(HashMap::new());
}

/// Shows a notification from the page while it is open (closed pages get Web Push through the
/// service worker); without permission nothing is shown. Through the worker's registration when
/// there is one: only those notifications have buttons, and their clicks reach the worker, which
/// hands them back to an open page (`teamcraft_core::web_push`). Otherwise the page's own.
async fn show_page_notification(notification: &OsNotification) -> Result<(), ActionFailure> {
  if notification_permission().as_deref() != Some("granted") {
    return Err(ActionFailure::new(
      FailureKind::Notification(NotificationFailure::NotAllowed),
      "notifications are not allowed in this browser",
    ));
  }
  let options = page_notification_options(notification);
  // The sender's or channel's picture (#543), from the media cache; a picture that can't be read
  // leaves the browser's default icon.
  if let Some(picture) = &notification.maybe_picture
    && let Some(icon) = crate::notification_pictures::icon_url(picture).await
  {
    drop(js_sys::Reflect::set(
      &options,
      &JsValue::from_str("icon"),
      &JsValue::from_str(&icon),
    ));
  }
  if show_through_worker(&notification.title, &options).await {
    return Ok(());
  }
  let Some(constructor) = notification_constructor() else {
    return Err(ActionFailure::new(
      FailureKind::Notification(NotificationFailure::Unsupported),
      "this browser has no notifications",
    ));
  };
  let args = js_sys::Array::of2(&JsValue::from_str(&notification.title), &options);
  let shown = js_sys::Reflect::construct(&constructor, &args).map_err(|_| {
    ActionFailure::new(
      FailureKind::Notification(NotificationFailure::Refused),
      "the browser refused the notification",
    )
  })?;
  let tag = notification.tag.as_str().to_owned();
  // async-ok: a thread-local, not a signal.
  PAGE_NOTIFICATIONS.with(|page| page.borrow_mut().insert(tag, shown));
  Ok(())
}

/// The options of `showNotification` / `new Notification`: body, tag, buttons, and the data the
/// worker's click handler reads (the conversation of a message, the tag of everything).
fn page_notification_options(notification: &OsNotification) -> js_sys::Object {
  let set = |object: &js_sys::Object, key: &str, value: &JsValue| {
    drop(js_sys::Reflect::set(object, &JsValue::from_str(key), value));
  };
  let options = js_sys::Object::new();
  if let Some(body) = &notification.maybe_body {
    set(&options, "body", &body.into());
  }
  set(&options, "tag", &notification.tag.as_str().into());
  let data = js_sys::Object::new();
  set(&data, "tag", &notification.tag.as_str().into());
  if let TagTarget::Conversation(conversation) = notification.tag.target() {
    set(&data, "conversation_id", &conversation.into());
  }
  set(&options, "data", &data);
  if notification.kind.is_ringing() {
    set(&options, "requireInteraction", &JsValue::TRUE);
  }
  // The page plays the message sound itself, or the user chose none (#800). A ringing call keeps
  // the system's sound: a tab that may not play sound can't ring.
  if notification.sound == NotificationSound::Silent {
    set(&options, "silent", &JsValue::TRUE);
  }
  let actions = js_sys::Array::new();
  for button in &notification.actions {
    let action = js_sys::Object::new();
    set(&action, "action", &button.action.key().into());
    set(&action, "title", &button.label.as_str().into());
    actions.push(&action);
  }
  if actions.length() > 0 {
    set(&options, "actions", &actions);
  }
  options
}

/// `registration.showNotification`; false when the page has no active service worker.
async fn show_through_worker(title: &str, options: &js_sys::Object) -> bool {
  let Some(window) = web_sys::window() else {
    return false;
  };
  let container = window.navigator().service_worker();
  if container.controller().is_none() {
    return false;
  }
  let Ok(ready) = container.ready() else {
    return false;
  };
  let Ok(registration) = JsFuture::from(ready).await else {
    return false;
  };
  let registration: web_sys::ServiceWorkerRegistration = registration.unchecked_into();
  let options: &web_sys::NotificationOptions = options.unchecked_ref();
  match registration.show_notification_with_options(title, options) {
    Ok(promise) => JsFuture::from(promise).await.is_ok(),
    Err(_) => false,
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
    assert_eq!(
      map_kind("refused", "the server refused the sign-in: refused".into()),
      AuthError::Denied("refused".into())
    );
    assert_eq!(
      map_kind("rate_limited", String::new()),
      AuthError::Denied("rate_limited".into())
    );
    assert!(matches!(
      map_kind("something_new", "x".into()),
      AuthError::InvalidResponse(_)
    ));
  }
}
