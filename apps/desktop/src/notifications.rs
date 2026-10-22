//! System notifications. The webview decides *whether* to notify (it holds the decrypted messages and
//! the user's preferences); this command only displays the result.

use serde::Serialize;
use tauri::AppHandle;
use tauri_plugin_notification::{NotificationExt, PermissionState};

#[derive(Debug, Serialize)]
pub struct NotifyError {
  kind: &'static str,
  message: String,
}

#[tauri::command]
pub fn notify_user(app: AppHandle, title: String, body: Option<String>) -> Result<(), NotifyError> {
  let notification = app.notification();
  if notification.permission_state().map_err(to_error)? != PermissionState::Granted
    && notification.request_permission().map_err(to_error)? != PermissionState::Granted
  {
    return Err(NotifyError {
      kind: "denied",
      message: "notifications are not allowed for this app".to_owned(),
    });
  }
  let mut builder = notification.builder().title(title);
  if let Some(body) = body {
    builder = builder.body(body);
  }
  builder.show().map_err(to_error)
}

fn to_error(error: tauri_plugin_notification::Error) -> NotifyError {
  NotifyError {
    kind: "failed",
    message: error.to_string(),
  }
}
