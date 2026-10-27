//! System notifications. The webview decides *whether* and *what* to show (it holds the decrypted
//! messages, the preferences and the language); these commands display, replace and withdraw the
//! result, and report what the user did with it as [`NOTIFICATION_ACTION_EVENT`]
//! (notifications.md, "Desktop notifications").
//!
//! | Platform | Shown through                        | Replace, withdraw | Buttons | Reply field                  |
//! |----------|--------------------------------------|-------------------|---------|------------------------------|
//! | Linux    | freedesktop notification service     | yes               | yes     | where the server offers one  |
//! | Windows  | WinRT toasts (tag and group)         | yes               | yes     | yes                          |
//! | macOS    | `UNUserNotificationCenter` (bundled) | yes               | yes     | yes                          |
//!
//! Where none of these is available (no notification service, an unbundled macOS build) the
//! notification plugin shows title and text only.

#[cfg(target_os = "macos")]
pub(crate) mod mac_center;
#[cfg(any(target_os = "macos", test))]
mod mac_response;
#[cfg(any(windows, test))]
mod toast_xml;
#[cfg(windows)]
mod win_toast;

use std::collections::HashMap;
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State, UserAttentionType};
use tauri_plugin_notification::NotificationExt;
use teamcraft_client_api::os_notify::{NOTIFICATION_ACTION_EVENT, NotificationActionEvent, OsNotification};

#[derive(Debug, Serialize)]
pub struct NotifyError {
  kind: &'static str,
  message: String,
}

/// Notifications the freedesktop service shows, by tag (Linux).
#[derive(Default)]
pub struct ShownNotifications {
  by_tag: Mutex<HashMap<String, Shown>>,
  /// Held from reading what is shown until the service answered with the ID: messages that
  /// arrive together would otherwise each see nothing shown and pop up separately.
  #[cfg(target_os = "linux")]
  showing: tokio::sync::Mutex<()>,
}

/// One notification on screen: the service's ID and what it shows, so the next message of the
/// same conversation can be added below the earlier ones.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct Shown {
  id: u32,
  title: String,
  lines: Vec<String>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn notify_user(
  app: AppHandle,
  shown: State<'_, ShownNotifications>,
  probe: State<'_, crate::attention::OsProbe>,
  notification: OsNotification,
) -> Result<(), NotifyError> {
  #[cfg(target_os = "linux")]
  if linux::notify(&app, &shown, &probe, &notification).await {
    return Ok(());
  }
  #[cfg(not(target_os = "linux"))]
  let _ = (&shown, &probe);
  #[cfg(windows)]
  {
    let reporter = app.clone();
    let on_action: win_toast::OnAction = std::sync::Arc::new(move |event| report(&reporter, event));
    match win_toast::show(&app.config().identifier, &notification, on_action) {
      Ok(()) => return Ok(()),
      Err(error) => tracing::debug!(%error, "toast failed, trying the plugin"),
    }
  }
  #[cfg(target_os = "macos")]
  if mac_center::is_available() {
    mac_center::show(&notification);
    return Ok(());
  }
  notify_plugin(&app, notification)
}

/// Removes the notifications with these tags.
#[tauri::command(rename_all = "snake_case")]
pub async fn withdraw_notifications(
  app: AppHandle,
  shown: State<'_, ShownNotifications>,
  probe: State<'_, crate::attention::OsProbe>,
  tags: Vec<String>,
) -> Result<(), ()> {
  let ids: Vec<u32> = {
    let mut by_tag = shown
      .by_tag
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner);
    tags
      .iter()
      .filter_map(|tag| by_tag.remove(tag))
      .map(|shown| shown.id)
      .collect()
  };
  #[cfg(target_os = "linux")]
  {
    let buses = probe.buses();
    for id in ids {
      buses.close(id).await;
    }
  }
  #[cfg(not(target_os = "linux"))]
  let _ = (ids, &probe);
  #[cfg(windows)]
  win_toast::withdraw(&app.config().identifier, &tags);
  #[cfg(target_os = "macos")]
  if mac_center::is_available() {
    mac_center::withdraw(&tags);
  }
  #[cfg(not(windows))]
  let _ = &app;
  #[cfg(not(any(windows, target_os = "macos")))]
  let _ = &tags;
  Ok(())
}

/// Asks for the user's attention while a call rings (Windows flashes the taskbar entry, macOS
/// bounces the dock icon, Linux sets the urgency hint) or stops asking.
#[tauri::command(rename_all = "snake_case")]
pub fn request_attention(app: AppHandle, is_wanted: bool) {
  let Some(window) = app.get_webview_window(crate::tray::MAIN_WINDOW) else {
    return;
  };
  let request = is_wanted.then_some(UserAttentionType::Critical);
  if let Err(error) = window.request_user_attention(request) {
    tracing::debug!(%error, "could not ask for attention");
  }
}

/// macOS asks the user once whether the app may notify and hears what the user does with the
/// notifications; elsewhere there is nothing to prepare.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub fn prepare(app: &AppHandle) {
  #[cfg(target_os = "macos")]
  if mac_center::is_available() {
    let reporter = app.clone();
    teamcraft_desktop_native::install_notification_delegate(move |response| {
      if let Some(event) = mac_response::action_event(response) {
        report(&reporter, event);
      }
    });
    mac_center::request_permission();
  }
}

/// Hands what the user did to the webview, bringing the window up first where the action needs it.
pub fn report(app: &AppHandle, event: NotificationActionEvent) {
  if event.action.brings_window_up() {
    crate::tray::show_main_window(app);
  }
  let payload = match serde_json::to_string(&event) {
    Ok(payload) => payload,
    Err(error) => {
      tracing::debug!(%error, "could not encode a notification action");
      return;
    },
  };
  if let Err(error) = app.emit(NOTIFICATION_ACTION_EVENT, payload) {
    tracing::debug!(%error, "could not tell the webview about a notification action");
  }
}

fn notify_plugin(app: &AppHandle, notification: OsNotification) -> Result<(), NotifyError> {
  let mut builder = app.notification().builder().title(notification.title);
  if let Some(body) = notification.maybe_body {
    builder = builder.body(body);
  }
  builder.show().map_err(|error| NotifyError {
    kind: "failed",
    message: error.to_string(),
  })
}

#[cfg(target_os = "linux")]
pub use linux::watch_notifications;

#[cfg(target_os = "linux")]
mod linux {
  use tauri::{AppHandle, Manager};
  use teamcraft_client_api::os_notify::{
    NotificationAction, NotificationActionEvent, NotificationTag, OsNotification, OsNotificationKind,
  };

  use super::{Shown, ShownNotifications};
  use crate::attention::OsProbe;
  use crate::freedesktop::{DesktopNotification, ServerEvent};

  /// Messages of one conversation shown together in a notification; older ones drop off the top.
  const MAX_STACKED_LINES: usize = 5;

  /// Reports clicks, buttons and inline replies on the app's notifications, and forgets the ones
  /// that are gone, for as long as the app runs.
  pub fn watch_notifications(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
      let buses = app.state::<OsProbe>().buses();
      let handle = app.clone();
      buses
        .watch_notifications(move |event: ServerEvent| {
          let shown = handle.state::<ShownNotifications>();
          let action = match event {
            ServerEvent::Action(action) => action,
            ServerEvent::Closed { id } => {
              shown.forget(id);
              return;
            },
          };
          let Some(tag) = shown.tag_of(action.id) else {
            return;
          };
          super::report(
            &handle,
            NotificationActionEvent {
              tag: NotificationTag::from_raw(tag),
              action: NotificationAction::from_key(&action.key),
              maybe_text: action.maybe_text,
            },
          );
        })
        .await;
    });
  }

  /// Shows (or replaces) a notification through the freedesktop service; false if there is none.
  pub async fn notify(
    app: &AppHandle,
    shown: &ShownNotifications,
    probe: &OsProbe,
    notification: &OsNotification,
  ) -> bool {
    let _showing = shown.showing.lock().await;
    let tag = notification.tag.as_str();
    let (maybe_previous, lines) = shown.next_lines(notification);
    let body = lines.join("\n");
    // A ringing call never expires and is updated in place. Everything else replaces its
    // predecessor with a new notification: KDE Plasma doesn't report a popup that timed out into
    // its history, and replacing that one only updates the history entry, without a popup (#531).
    let replaces_id = match maybe_previous {
      Some(previous) if notification.kind.is_ringing() => previous,
      Some(previous) => {
        probe.buses().close(previous).await;
        0
      },
      None => 0,
    };
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
    let buttons: Vec<(&str, &str)> = notification
      .actions
      .iter()
      .map(|button| (button.action.key(), button.label.as_str()))
      .collect();
    let desktop = DesktopNotification {
      app_name: &app_name,
      app_icon: &app_icon,
      maybe_desktop_entry: crate::desktop_entry::installed(app),
      title: &notification.title,
      body: &body,
      replaces_id,
      actions: &buttons,
      maybe_reply_placeholder: notification
        .maybe_reply
        .as_ref()
        .map(|reply| reply.placeholder.as_str()),
      category: category(notification.kind),
      is_ringing: notification.kind.is_ringing(),
    };
    let Some(id) = probe.buses().notify(&desktop).await else {
      return false;
    };
    shown.remember(tag, id, &notification.title, lines);
    true
  }

  /// The notification spec's category, which servers use for sounds, icons and grouping.
  fn category(kind: OsNotificationKind) -> &'static str {
    match kind {
      OsNotificationKind::Message => "im.received",
      OsNotificationKind::IncomingCall => "call.incoming",
      OsNotificationKind::MissedCall => "call.unanswered",
      OsNotificationKind::MeetingReminder
      | OsNotificationKind::Reminder
      | OsNotificationKind::JoinRequest
      | OsNotificationKind::Unknown => "",
    }
  }

  impl ShownNotifications {
    /// The notification of the same tag that `notification` follows, if one is still shown or in
    /// the history, and the lines of its body. A message following one with the same title (the
    /// same sender, in the same conversation) is added below the earlier ones, as messaging apps
    /// do; the oldest go first.
    pub(super) fn next_lines(&self, notification: &OsNotification) -> (Option<u32>, Vec<String>) {
      let by_tag = self
        .by_tag
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
      let maybe_shown = by_tag.get(notification.tag.as_str());
      let mut lines = match maybe_shown {
        Some(shown)
          if notification.kind == OsNotificationKind::Message && shown.title == notification.title =>
        {
          shown.lines.clone()
        },
        _ => Vec::new(),
      };
      lines.extend(notification.maybe_body.clone());
      let excess = lines.len().saturating_sub(MAX_STACKED_LINES);
      lines.drain(..excess);
      (maybe_shown.map(|shown| shown.id), lines)
    }

    pub(super) fn remember(&self, tag: &str, id: u32, title: &str, lines: Vec<String>) {
      self
        .by_tag
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
          tag.to_owned(),
          Shown {
            id,
            title: title.to_owned(),
            lines,
          },
        );
    }

    /// Drops a notification that is gone (dismissed, closed), so the next one of its tag starts
    /// afresh.
    pub(super) fn forget(&self, id: u32) {
      self
        .by_tag
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|_, shown| shown.id != id);
    }

    /// The tag of a notification the service showed under `id`.
    fn tag_of(&self, id: u32) -> Option<String> {
      self
        .by_tag
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|(_, shown)| shown.id == id)
        .map(|(tag, _)| tag.clone())
    }
  }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
  use teamcraft_client_api::os_notify::{NotificationTag, OsNotification, OsNotificationKind};

  use super::ShownNotifications;

  const TAG: &str = "conversation:1";

  #[test]
  fn a_closed_notification_is_forgotten() {
    let shown = ShownNotifications::default();
    shown.remember(TAG, 7, "Bob", vec!["hello".to_owned()]);
    assert_eq!(shown.next_lines(&message("Bob", "again")).0, Some(7));
    shown.forget(8);
    assert_eq!(
      shown.next_lines(&message("Bob", "again")).0,
      Some(7),
      "another notification closed"
    );
    shown.forget(7);
    assert_eq!(
      shown.next_lines(&message("Bob", "again")),
      (None, vec!["again".to_owned()])
    );
  }

  #[test]
  fn messages_stack_and_the_oldest_drop_off() {
    let shown = ShownNotifications::default();
    for index in 1..=6 {
      let (_, lines) = shown.next_lines(&message("Bob", &format!("message {index}")));
      shown.remember(TAG, index, "Bob", lines);
    }
    let (maybe_previous, lines) = shown.next_lines(&message("Bob", "message 7"));
    assert_eq!(maybe_previous, Some(6));
    assert_eq!(
      lines,
      ["message 3", "message 4", "message 5", "message 6", "message 7"]
    );
  }

  #[test]
  fn another_sender_or_kind_starts_a_new_body() {
    let shown = ShownNotifications::default();
    shown.remember(TAG, 3, "Bob in #general", vec!["hi".to_owned()]);
    assert_eq!(
      shown.next_lines(&message("Carol in #general", "hey")),
      (Some(3), vec!["hey".to_owned()])
    );
    let mut call = message("Bob in #general", "Calling");
    call.kind = OsNotificationKind::IncomingCall;
    assert_eq!(shown.next_lines(&call), (Some(3), vec!["Calling".to_owned()]));
  }

  fn message(title: &str, body: &str) -> OsNotification {
    OsNotification {
      tag: NotificationTag::from_raw(TAG.to_owned()),
      kind: OsNotificationKind::Message,
      title: title.to_owned(),
      maybe_body: Some(body.to_owned()),
      actions: Vec::new(),
      maybe_reply: None,
    }
  }
}
