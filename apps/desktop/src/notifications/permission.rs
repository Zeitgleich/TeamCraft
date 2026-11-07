//! Whether the system lets the app notify, and asking for it (#895). The webview shows the banner
//! and Settings > Notifications > "This device"; these commands read the system's answer and, on
//! macOS, ask for permission from the user's click on "Turn on" (never at start-up, so the prompt
//! cannot hold up the start and comes when the user knows why). Windows and Linux have nothing to
//! ask: the webview shows a test notification through `notify_user` instead.
//!
//! | Platform | Read from                                      | Asked by `notification_allow` |
//! |----------|------------------------------------------------|-------------------------------|
//! | macOS    | `UNUserNotificationCenter` settings (bundled)  | `requestAuthorization`        |
//! | Windows  | the toast notifier's setting for the app's ID  | nothing                       |
//! | Linux    | whether a notification service answers         | nothing                       |

use tauri::AppHandle;
use teamcraft_client_api::os_notify::{DesktopNotifyState, NotifyPermission, NotifyPlatform};

use super::NotifyError;

/// What the system says now. Read again whenever the window comes to the front, so a change in
/// the system's settings shows.
#[tauri::command(rename_all = "snake_case")]
pub async fn notification_state(
  app: AppHandle,
  probe: tauri::State<'_, crate::attention::OsProbe>,
) -> Result<DesktopNotifyState, ()> {
  let permission = read(&app, &probe).await;
  Ok(state(&app, permission))
}

/// Asks for permission where the system has a prompt (macOS) and answers with the state after it.
/// `Err` only where the system failed to answer.
#[tauri::command(rename_all = "snake_case")]
pub async fn notification_allow(
  app: AppHandle,
  probe: tauri::State<'_, crate::attention::OsProbe>,
) -> Result<DesktopNotifyState, NotifyError> {
  #[cfg(target_os = "macos")]
  if super::mac_center::is_available() {
    let is_granted = super::mac_center::request_permission()
      .await
      .map_err(|message| NotifyError {
        kind: "failed",
        message,
      })?;
    let permission = if is_granted {
      NotifyPermission::Allowed
    } else {
      NotifyPermission::Blocked
    };
    return Ok(state(&app, permission));
  }
  let permission = read(&app, &probe).await;
  Ok(state(&app, permission))
}

fn state(app: &AppHandle, permission: NotifyPermission) -> DesktopNotifyState {
  DesktopNotifyState {
    permission,
    platform: platform(),
    app_name: app
      .config()
      .product_name
      .clone()
      .unwrap_or_else(|| "TeamCraft".to_owned()),
  }
}

fn platform() -> NotifyPlatform {
  if cfg!(target_os = "macos") {
    NotifyPlatform::MacOs
  } else if cfg!(windows) {
    NotifyPlatform::Windows
  } else if cfg!(target_os = "linux") {
    NotifyPlatform::Linux
  } else {
    NotifyPlatform::Other
  }
}

// `app` serves Windows only, `probe` Linux only.
#[allow(unused_variables)]
async fn read(app: &AppHandle, probe: &crate::attention::OsProbe) -> NotifyPermission {
  #[cfg(target_os = "macos")]
  {
    // An unbundled development build has no notification center (asking for it crashes).
    if !super::mac_center::is_available() {
      return NotifyPermission::Unavailable;
    }
    super::mac_center::permission().await
  }
  #[cfg(windows)]
  {
    windows_permission(&app.config().identifier)
  }
  #[cfg(target_os = "linux")]
  {
    if probe.buses().has_notification_server().await {
      NotifyPermission::Allowed
    } else {
      NotifyPermission::Unavailable
    }
  }
  #[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
  {
    NotifyPermission::Unknown
  }
}

/// The toast notifier's setting for the app: off for the app or for all apps, or by policy.
#[cfg(windows)]
fn windows_permission(app_id: &str) -> NotifyPermission {
  use windows::UI::Notifications::{NotificationSetting, ToastNotificationManager};
  use windows::core::HSTRING;
  let setting = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(app_id))
    .and_then(|notifier| notifier.Setting());
  match setting {
    Ok(NotificationSetting::Enabled) => NotifyPermission::Allowed,
    Ok(_) => NotifyPermission::Blocked,
    Err(error) => {
      // An unregistered app ID (a development run) has no setting; the plugin still shows its
      // notifications, which the test notification checks.
      tracing::debug!(%error, "no toast setting for the app");
      NotifyPermission::Allowed
    },
  }
}
