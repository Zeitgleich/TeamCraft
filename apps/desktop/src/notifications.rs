//! System notifications. The webview decides *whether* to notify (it holds the decrypted messages and
//! the user's preferences); these commands only display and withdraw the result.
//!
//! A notification can carry a tag (the conversation ID): a newer one with the same tag replaces it,
//! and `withdraw_notifications` removes it once the conversation was read. On Linux both go through
//! the freedesktop notification service directly. The notification plugin can neither replace nor
//! remove a shown notification on macOS and Windows, so there they stay until the user dismisses them.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, State};
use tauri_plugin_notification::{NotificationExt, PermissionState};

#[derive(Debug, Serialize)]
pub struct NotifyError {
  kind: &'static str,
  message: String,
}

/// Shown notifications by tag (Linux; IDs of the notification service).
#[derive(Default)]
pub struct ShownNotifications {
  by_tag: Mutex<HashMap<String, u32>>,
}

/// Emitted to the webview with the tag (conversation ID) of a clicked notification.
#[cfg(target_os = "linux")]
pub const NOTIFICATION_OPENED_EVENT: &str = "notification-opened";

#[cfg(target_os = "linux")]
impl ShownNotifications {
  /// The tag of a notification the service showed under `id`.
  fn tag_of(&self, id: u32) -> Option<String> {
    self
      .by_tag
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .iter()
      .find(|(_, shown)| **shown == id)
      .map(|(tag, _)| tag.clone())
  }
}

/// Linux: a click on one of our notifications brings the window up and opens its conversation.
/// (macOS and Windows: the notification plugin reports no clicks; the OS just focuses the app.)
#[cfg(target_os = "linux")]
pub fn watch_clicks(app: &AppHandle) {
  use tauri::{Emitter, Manager};

  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    let buses = app.state::<crate::attention::OsProbe>().buses();
    let handle = app.clone();
    buses
      .watch_clicks(move |id| {
        let Some(tag) = handle.state::<ShownNotifications>().tag_of(id) else {
          return;
        };
        crate::tray::show_main_window(&handle);
        if let Err(error) = handle.emit(NOTIFICATION_OPENED_EVENT, tag) {
          tracing::debug!(%error, "could not tell the webview about the click");
        }
      })
      .await;
  });
}

#[tauri::command]
pub async fn notify_user(
  app: AppHandle,
  shown: State<'_, ShownNotifications>,
  probe: State<'_, crate::attention::OsProbe>,
  title: String,
  body: Option<String>,
  tag: Option<String>,
) -> Result<(), NotifyError> {
  #[cfg(target_os = "linux")]
  if notify_freedesktop(&app, &shown, &probe, &title, body.as_deref(), tag.as_deref()).await {
    return Ok(());
  }
  #[cfg(not(target_os = "linux"))]
  let _ = (shown, probe, tag);
  notify_plugin(&app, title, body)
}

/// Removes the notifications with these tags where the platform allows (Linux).
#[tauri::command]
pub async fn withdraw_notifications(
  shown: State<'_, ShownNotifications>,
  probe: State<'_, crate::attention::OsProbe>,
  tags: Vec<String>,
) -> Result<(), ()> {
  let ids: Vec<u32> = {
    let mut by_tag = shown
      .by_tag
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    tags.iter().filter_map(|tag| by_tag.remove(tag)).collect()
  };
  #[cfg(target_os = "linux")]
  {
    let buses = probe.buses();
    for id in ids {
      buses.close(id).await;
    }
  }
  #[cfg(not(target_os = "linux"))]
  let _ = (ids, probe);
  Ok(())
}

/// Shows (or replaces) a notification through the freedesktop service; false if there is none.
#[cfg(target_os = "linux")]
async fn notify_freedesktop(
  app: &AppHandle,
  shown: &ShownNotifications,
  probe: &crate::attention::OsProbe,
  title: &str,
  maybe_body: Option<&str>,
  maybe_tag: Option<&str>,
) -> bool {
  let replaces_id = maybe_tag.map_or(0, |tag| {
    shown
      .by_tag
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(tag)
      .copied()
      .unwrap_or(0)
  });
  let config = app.config();
  let app_name = config
    .product_name
    .clone()
    .unwrap_or_else(|| "TeamCraft".to_owned());
  // The icon the plugin uses too: the executable's name, which the package installs as icon name.
  let app_icon = std::env::current_exe()
    .ok()
    .and_then(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned()))
    .unwrap_or_default();
  let notification = crate::freedesktop::DesktopNotification {
    app_name: &app_name,
    app_icon: &app_icon,
    desktop_entry: &config.identifier,
    title,
    body: maybe_body.unwrap_or_default(),
    replaces_id,
  };
  let Some(id) = probe.buses().notify(&notification).await else {
    return false;
  };
  if let Some(tag) = maybe_tag {
    shown
      .by_tag
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(tag.to_owned(), id);
  }
  true
}

fn notify_plugin(app: &AppHandle, title: String, body: Option<String>) -> Result<(), NotifyError> {
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
