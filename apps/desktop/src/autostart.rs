//! "Start at login" for the desktop app. The UI shows the switch; the shell owns the OS entry, so
//! the webview needs no plugin permission (the plugin's own commands are not exposed to it).
//! Windows only for now (a per-user registry Run entry); other platforms report it as unsupported.

use serde::Serialize;
use tauri::AppHandle;

#[derive(Debug, Serialize)]
pub struct AutostartError {
  kind: &'static str,
  message: String,
}

/// Whether the app starts when the user logs in.
#[tauri::command]
pub fn autostart_get(app: AppHandle) -> Result<bool, AutostartError> {
  platform::is_enabled(&app)
}

#[tauri::command]
pub fn autostart_set(app: AppHandle, enabled: bool) -> Result<(), AutostartError> {
  platform::set_enabled(&app, enabled)
}

#[cfg(windows)]
mod platform {
  use tauri::AppHandle;
  use tauri_plugin_autostart::ManagerExt;

  use super::AutostartError;

  pub(super) fn is_enabled(app: &AppHandle) -> Result<bool, AutostartError> {
    app.autolaunch().is_enabled().map_err(failed)
  }

  pub(super) fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), AutostartError> {
    let launcher = app.autolaunch();
    if enabled {
      launcher.enable()
    } else {
      launcher.disable()
    }
    .map_err(failed)
  }

  fn failed(error: impl std::fmt::Display) -> AutostartError {
    AutostartError {
      kind: "failed",
      message: error.to_string(),
    }
  }
}

#[cfg(not(windows))]
mod platform {
  use tauri::AppHandle;

  use super::AutostartError;

  pub(super) fn is_enabled(_app: &AppHandle) -> Result<bool, AutostartError> {
    Err(unsupported())
  }

  pub(super) fn set_enabled(_app: &AppHandle, _enabled: bool) -> Result<(), AutostartError> {
    Err(unsupported())
  }

  fn unsupported() -> AutostartError {
    AutostartError {
      kind: "unsupported",
      message: "starting at login is not supported on this platform yet".to_owned(),
    }
  }
}
