//! "Send to TeamCraft" in the Linux file managers' context menus (#1393). The switch is in Settings;
//! the shell owns the files, per user, so everyone can turn it off and nothing is installed
//! system-wide. An entry runs `teamcraft-desktop --share -- <paths>` (shared_files.rs).
//!
//! | File manager      | Entry                                                                      |
//! |-------------------|----------------------------------------------------------------------------|
//! | Dolphin (KDE)     | service menu `~/.local/share/kio/servicemenus/…send-to.desktop`            |
//! | Files (GNOME)     | script `~/.local/share/nautilus/scripts/<name>` (menu "Scripts")           |
//! | Nemo (Cinnamon)   | action `~/.local/share/nemo/actions/…send-to.nemo_action`                  |
//! | Caja (MATE)       | script `~/.config/caja/scripts/<name>`                                     |
//! | Thunar (Xfce)     | custom action merged into `~/.config/Thunar/uca.xml`, found by its id      |
//!
//! Only files carrying [`MARKER`] are ever changed or removed; a file of the same name that the user
//! or another program wrote stays as it is. The entries are written when the switch turns on and
//! again at every start, so a moved AppImage or an updated install path is repaired, and removed
//! when it turns off. A manager that isn't installed gets no entry (and no directory).
//!
//! The `Exec` follows the packaging: deb and rpm run `teamcraft-desktop`, the AppImage its own file
//! (not its temporary mount), the Flatpak `flatpak run --file-forwarding`, which hands the files to
//! the sandbox through the document portal.

use serde::Serialize;
use tauri::AppHandle;

#[cfg(target_os = "linux")]
mod linux;

/// Why the entries couldn't be written or removed; reaches the UI as its message (a toast).
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct FileManagerEntriesError(String);

impl FileManagerEntriesError {
  fn new(error: impl std::fmt::Display) -> Self {
    Self(format!("file manager entry: {error}"))
  }
}

/// Whether the entries are switched on; `None` outside Linux, where the setting is hidden.
#[tauri::command(rename_all = "snake_case")]
pub async fn file_manager_entries_get(app: AppHandle) -> Result<Option<bool>, FileManagerEntriesError> {
  Ok(platform::is_enabled(&app))
}

/// Switches the entries on or off and answers the state afterwards.
#[tauri::command(rename_all = "snake_case")]
pub async fn file_manager_entries_set(
  app: AppHandle,
  enabled: bool,
) -> Result<bool, FileManagerEntriesError> {
  platform::set_enabled(&app, enabled).await?;
  Ok(enabled)
}

/// At start: writes the entries again when they are on (an updated or moved install), so they never
/// point at a path that is gone. No UI is there to show a failure; it goes to the log.
pub fn repair(app: &AppHandle) {
  platform::repair(app);
}

#[cfg(target_os = "linux")]
use linux as platform;

#[cfg(not(target_os = "linux"))]
mod platform {
  use tauri::AppHandle;

  use super::FileManagerEntriesError;

  pub(super) fn is_enabled(_app: &AppHandle) -> Option<bool> {
    None
  }

  pub(super) async fn set_enabled(_app: &AppHandle, _enabled: bool) -> Result<(), FileManagerEntriesError> {
    Err(FileManagerEntriesError::new("not supported on this platform"))
  }

  pub(super) fn repair(_app: &AppHandle) {}
}
