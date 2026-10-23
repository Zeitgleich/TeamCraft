//! System notifications. The webview decides *whether* and *what* to show (it holds the decrypted
//! messages, the preferences and the language); these commands display, replace and withdraw the
//! result, and report what the user did with it as [`NOTIFICATION_ACTION_EVENT`]
//! (notifications.md, "Desktop notifications").
//!
//! | Platform | Shown through                        | Replace, withdraw | Buttons | Reply field                  |
//! |----------|--------------------------------------|-------------------|---------|------------------------------|
//! | Linux    | freedesktop notification service     | yes               | yes     | where the server offers one  |
//! | Windows  | WinRT toasts (tag and group)         | yes               | yes     | yes                          |
//! | macOS    | `UNUserNotificationCenter` (bundled) | yes               | no      | no                           |
//!
//! Where none of these is available (no notification service, an unbundled macOS build) the
//! notification plugin shows title and text only.

#[cfg(target_os = "macos")]
mod mac_center;
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

/// Notifications the freedesktop service shows, by tag (Linux; the service's IDs).
#[derive(Default)]
pub struct ShownNotifications {
  by_tag: Mutex<HashMap<String, u32>>,
}

#[tauri::command]
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
#[tauri::command]
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
  let _ = (ids, &probe);
  #[cfg(windows)]
  win_toast::withdraw(&app.config().identifier, &tags);
  #[cfg(target_os = "macos")]
  if mac_center::is_available() {
    mac_center::withdraw(&tags);
  }
  #[cfg(not(any(windows, target_os = "macos")))]
  let _ = (&app, &tags);
  Ok(())
}

/// Asks for the user's attention while a call rings (Windows flashes the taskbar entry, macOS
/// bounces the dock icon, Linux sets the urgency hint) or stops asking.
#[tauri::command]
pub fn request_attention(app: AppHandle, is_wanted: bool) {
  let Some(window) = app.get_webview_window(crate::tray::MAIN_WINDOW) else {
    return;
  };
  let request = is_wanted.then_some(UserAttentionType::Critical);
  if let Err(error) = window.request_user_attention(request) {
    tracing::debug!(%error, "could not ask for attention");
  }
}

/// macOS asks the user once whether the app may notify; elsewhere there is nothing to ask.
pub fn prepare() {
  #[cfg(target_os = "macos")]
  if mac_center::is_available() {
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
pub use linux::watch_actions;

#[cfg(target_os = "linux")]
mod linux {
  use std::path::PathBuf;
  use std::sync::OnceLock;

  use tauri::{AppHandle, Manager};
  use teamcraft_client_api::os_notify::{
    NotificationAction, NotificationActionEvent, NotificationTag, OsNotification, OsNotificationKind,
  };

  use super::ShownNotifications;
  use crate::attention::OsProbe;
  use crate::freedesktop::{DesktopNotification, ServerAction};

  /// Reports clicks, buttons and inline replies on the app's notifications for as long as the app
  /// runs.
  pub fn watch_actions(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
      let buses = app.state::<OsProbe>().buses();
      let handle = app.clone();
      buses
        .watch_actions(move |action: ServerAction| {
          let Some(tag) = handle.state::<ShownNotifications>().tag_of(action.id) else {
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
    let tag = notification.tag.as_str();
    let replaces_id = shown
      .by_tag
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .get(tag)
      .copied()
      .unwrap_or(0);
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
      maybe_desktop_entry: desktop_entry(app).as_deref(),
      title: &notification.title,
      body: notification.maybe_body.as_deref().unwrap_or_default(),
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
    shown
      .by_tag
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
      .insert(tag.to_owned(), id);
    true
  }

  /// The `.desktop` entry the packages installed for this app, looked up once: the Flatpak's ID,
  /// the identifier, the product name (Tauri's deb, rpm and AppImage name it `TeamCraft.desktop`)
  /// or the executable's name.
  fn desktop_entry(app: &AppHandle) -> &'static Option<String> {
    static ENTRY: OnceLock<Option<String>> = OnceLock::new();
    ENTRY.get_or_init(|| {
      let config = app.config();
      let mut candidates: Vec<String> = std::env::var("FLATPAK_ID").into_iter().collect();
      candidates.push(config.identifier.clone());
      candidates.extend(config.product_name.clone());
      candidates.extend(
        std::env::current_exe()
          .ok()
          .and_then(|path| path.file_stem().map(|stem| stem.to_string_lossy().into_owned())),
      );
      let entry = installed_entry(&candidates, &application_dirs());
      if entry.is_none() {
        tracing::debug!("no installed .desktop entry: notifications name the app by its name only");
      }
      entry
    })
  }

  /// The first candidate with a `<name>.desktop` in one of `dirs`.
  pub(super) fn installed_entry(candidates: &[String], dirs: &[PathBuf]) -> Option<String> {
    candidates
      .iter()
      .find(|name| {
        dirs
          .iter()
          .any(|dir| dir.join(format!("{name}.desktop")).is_file())
      })
      .cloned()
  }

  /// `applications` below the XDG data directories, user first, as desktop environments search.
  fn application_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
      .map(PathBuf::from)
      .or_else(|| home.as_ref().map(|home| home.join(".local/share")));
    let data_dirs =
      std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".to_owned());
    data_home
      .into_iter()
      .chain(
        data_dirs
          .split(':')
          .filter(|dir| !dir.is_empty())
          .map(PathBuf::from),
      )
      .map(|dir| dir.join("applications"))
      .collect()
  }

  /// The notification spec's category, which servers use for sounds, icons and grouping.
  fn category(kind: OsNotificationKind) -> &'static str {
    match kind {
      OsNotificationKind::Message => "im.received",
      OsNotificationKind::IncomingCall => "call.incoming",
      OsNotificationKind::MissedCall => "call.unanswered",
      OsNotificationKind::MeetingReminder | OsNotificationKind::Reminder | OsNotificationKind::Unknown => "",
    }
  }

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
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
  use super::linux::installed_entry;

  #[test]
  fn the_desktop_entry_is_the_one_actually_installed() {
    let dir = std::env::temp_dir().join(format!("teamcraft-desktop-entry-{}", std::process::id()));
    let applications = dir.join("applications");
    std::fs::create_dir_all(&applications).unwrap();
    let candidates = ["de.zeitgleich.teamcraft".to_owned(), "TeamCraft".to_owned()];
    assert_eq!(
      installed_entry(&candidates, std::slice::from_ref(&applications)),
      None
    );
    std::fs::write(applications.join("TeamCraft.desktop"), "[Desktop Entry]\n").unwrap();
    assert_eq!(
      installed_entry(&candidates, std::slice::from_ref(&applications)).as_deref(),
      Some("TeamCraft")
    );
    std::fs::write(
      applications.join("de.zeitgleich.teamcraft.desktop"),
      "[Desktop Entry]\n",
    )
    .unwrap();
    assert_eq!(
      installed_entry(&candidates, &[applications]).as_deref(),
      Some("de.zeitgleich.teamcraft")
    );
    std::fs::remove_dir_all(dir).unwrap();
  }
}
