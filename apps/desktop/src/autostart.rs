//! "Start at login" for the desktop app (#676). The UI shows the switch; the shell owns the OS entry,
//! so the webview needs no plugin permission (the plugin's own commands are not exposed to it).
//!
//! - Windows: a per-user registry Run entry (autostart plugin).
//! - macOS: a LaunchAgent in `~/Library/LaunchAgents` (autostart plugin).
//! - Linux (deb, rpm, AppImage): an XDG autostart entry written here, so its `Exec` names the real
//!   program: the AppImage file itself, not its temporary mount.
//! - Flatpak: the sandbox can't write that entry; the Background portal does, after asking the user.
//!   The portal can't be asked what it set, so the last answer is remembered.
//!
//! Started at login, the app comes up in the tray ([`AUTOSTART_ARG`]). Where it can't work the
//! switch is hidden: `autostart_get` answers `None`.

use serde::Serialize;
use tauri::AppHandle;

/// The argument the login entry starts the app with: it comes up in the tray, not in front of the
/// user.
pub const AUTOSTART_ARG: &str = "--autostart";

/// Why starting at login couldn't be read or changed; reaches the UI as its message (a toast).
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct AutostartError(String);

/// Whether the app starts when the user logs in; `None` where the platform can't do it.
#[tauri::command(rename_all = "snake_case")]
pub async fn autostart_get(app: AppHandle) -> Result<Option<bool>, AutostartError> {
  platform::is_enabled(&app).await
}

/// Turns starting at login on or off and answers the state read back afterwards (the user may have
/// refused the Flatpak portal's question).
#[tauri::command(rename_all = "snake_case")]
pub async fn autostart_set(app: AppHandle, enabled: bool) -> Result<bool, AutostartError> {
  platform::set_enabled(&app, enabled).await?;
  platform::is_enabled(&app)
    .await?
    .ok_or_else(|| failed("starting at login is not supported here"))
}

/// This launch came from the login entry.
pub fn is_login_launch() -> bool {
  std::env::args_os().any(|arg| arg == AUTOSTART_ARG)
}

fn failed(error: impl std::fmt::Display) -> AutostartError {
  AutostartError(format!("start at login: {error}"))
}

#[cfg(any(windows, target_os = "macos"))]
mod platform {
  use tauri::AppHandle;
  use tauri_plugin_autostart::ManagerExt;

  use super::{AutostartError, failed};

  pub(super) async fn is_enabled(app: &AppHandle) -> Result<Option<bool>, AutostartError> {
    app.autolaunch().is_enabled().map(Some).map_err(failed)
  }

  pub(super) async fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), AutostartError> {
    let launcher = app.autolaunch();
    if enabled {
      launcher.enable()
    } else {
      launcher.disable()
    }
    .map_err(failed)
  }
}

#[cfg(target_os = "linux")]
mod platform {
  use tauri::{AppHandle, Manager};

  use super::{AutostartError, failed, xdg};

  pub(super) async fn is_enabled(app: &AppHandle) -> Result<Option<bool>, AutostartError> {
    if xdg::is_flatpak() {
      return Ok(Some(super::portal::remembered(app)));
    }
    let path = xdg::entry_path(app).map_err(failed)?;
    let is_on = xdg::is_enabled(&path).map_err(failed)?;
    if is_on {
      // The program may have moved since (an AppImage updated or saved elsewhere): the entry
      // follows it.
      xdg::write_entry(&path, &launch_path(app)?).map_err(failed)?;
    }
    Ok(Some(is_on))
  }

  pub(super) async fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), AutostartError> {
    if xdg::is_flatpak() {
      return super::portal::request(app, enabled).await;
    }
    let path = xdg::entry_path(app).map_err(failed)?;
    match enabled {
      true => xdg::write_entry(&path, &launch_path(app)?),
      false => xdg::remove_entry(&path),
    }
    .map_err(failed)
  }

  /// The program to start: the AppImage file when running from one, else this binary.
  fn launch_path(app: &AppHandle) -> Result<std::path::PathBuf, AutostartError> {
    if let Some(appimage) = app.env().appimage {
      return Ok(appimage.into());
    }
    std::env::current_exe().map_err(failed)
  }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
  use tauri::AppHandle;

  use super::AutostartError;

  pub(super) async fn is_enabled(_app: &AppHandle) -> Result<Option<bool>, AutostartError> {
    Ok(None)
  }

  pub(super) async fn set_enabled(_app: &AppHandle, _enabled: bool) -> Result<(), AutostartError> {
    Err(super::failed(
      "starting at login is not supported on this platform",
    ))
  }
}

/// The XDG autostart entry (Linux outside Flatpak).
#[cfg(target_os = "linux")]
mod xdg {
  use std::fs;
  use std::io;
  use std::path::{Path, PathBuf};

  use tauri::{AppHandle, Manager};

  use super::AUTOSTART_ARG;

  /// The entry's file name: the app ID, like the installed desktop entry.
  const ENTRY_FILE: &str = "de.zeitgleich.teamcraft.desktop";

  pub fn is_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists()
  }

  /// `$XDG_CONFIG_HOME/autostart/de.zeitgleich.teamcraft.desktop`.
  pub fn entry_path(app: &AppHandle) -> tauri::Result<PathBuf> {
    Ok(app.path().config_dir()?.join("autostart").join(ENTRY_FILE))
  }

  /// The entry is there and not switched off (`Hidden=true`, or GNOME's own flag set to false by its
  /// settings).
  pub fn is_enabled(path: &Path) -> io::Result<bool> {
    match fs::read_to_string(path) {
      Ok(entry) => Ok(!is_switched_off(&entry)),
      Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
      Err(error) => Err(error),
    }
  }

  /// Writes the entry unless it already says exactly this.
  pub fn write_entry(path: &Path, program: &Path) -> io::Result<()> {
    let entry = entry(program);
    if fs::read_to_string(path).is_ok_and(|existing| existing == entry) {
      return Ok(());
    }
    if let Some(directory) = path.parent() {
      fs::create_dir_all(directory)?;
    }
    fs::write(path, entry)
  }

  pub fn remove_entry(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
      Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
      _ => Ok(()),
    }
  }

  /// The desktop entry that starts `program` in the tray.
  pub fn entry(program: &Path) -> String {
    format!(
      "[Desktop Entry]\n\
       Type=Application\n\
       Name=TeamCraft\n\
       Comment=Team chat, calls and meetings\n\
       Exec={} {AUTOSTART_ARG}\n\
       Icon=de.zeitgleich.teamcraft\n\
       Terminal=false\n\
       X-GNOME-Autostart-enabled=true\n",
      exec_argument(&program.to_string_lossy())
    )
  }

  /// One argument of an `Exec` key: quoted, with the characters the desktop entry spec reserves
  /// inside quotes escaped (then backslashes escaped once more for the string value), and `%` doubled
  /// so it isn't a field code.
  pub fn exec_argument(argument: &str) -> String {
    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    for character in argument.chars() {
      match character {
        '"' | '`' | '$' => {
          quoted.push_str("\\\\");
          quoted.push(character);
        },
        '\\' => quoted.push_str("\\\\\\\\"),
        '%' => quoted.push_str("%%"),
        other => quoted.push(other),
      }
    }
    quoted.push('"');
    quoted
  }

  fn is_switched_off(entry: &str) -> bool {
    entry.lines().map(str::trim).any(|line| {
      line.eq_ignore_ascii_case("Hidden=true") || line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    })
  }

  #[cfg(test)]
  mod tests {
    use super::*;

    #[test]
    fn the_entry_starts_the_installed_binary_in_the_tray() {
      let entry = entry(Path::new("/usr/bin/teamcraft-desktop"));
      assert!(entry.starts_with("[Desktop Entry]\n"));
      assert!(entry.contains("\nExec=\"/usr/bin/teamcraft-desktop\" --autostart\n"));
      assert!(entry.contains("\nType=Application\n"));
      assert!(!is_switched_off(&entry));
    }

    #[test]
    fn an_appimage_is_started_from_its_file() {
      let entry = entry(Path::new("/home/user/Apps/TeamCraft_1.0.7_amd64.AppImage"));
      assert!(entry.contains("\nExec=\"/home/user/Apps/TeamCraft_1.0.7_amd64.AppImage\" --autostart\n"));
    }

    #[test]
    fn reserved_characters_in_the_path_are_escaped() {
      assert_eq!(exec_argument("/opt/My Apps/team"), "\"/opt/My Apps/team\"");
      assert_eq!(exec_argument("/a\"b"), "\"/a\\\\\"b\"");
      assert_eq!(exec_argument("/a$b`c"), "\"/a\\\\$b\\\\`c\"");
      assert_eq!(exec_argument("/a\\b"), "\"/a\\\\\\\\b\"");
      assert_eq!(exec_argument("/100%/app"), "\"/100%%/app\"");
    }

    #[test]
    fn an_entry_switched_off_in_the_desktop_settings_counts_as_off() {
      assert!(is_switched_off("[Desktop Entry]\nHidden=true\n"));
      assert!(is_switched_off(
        "[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"
      ));
    }

    #[test]
    fn writing_and_removing_the_entry() {
      let directory = std::env::temp_dir().join(format!("teamcraft-autostart-{}", std::process::id()));
      let path = directory.join("autostart").join(ENTRY_FILE);
      assert!(!is_enabled(&path).expect("readable"));
      write_entry(&path, Path::new("/usr/bin/teamcraft-desktop")).expect("written");
      assert!(is_enabled(&path).expect("readable"));
      remove_entry(&path).expect("removed");
      remove_entry(&path).expect("removing twice is fine");
      assert!(!is_enabled(&path).expect("readable"));
      fs::remove_dir_all(&directory).expect("cleaned up");
    }
  }
}

/// The Background portal (Flatpak): it writes the autostart entry outside the sandbox.
#[cfg(target_os = "linux")]
mod portal {
  use std::fs;
  use std::path::PathBuf;

  use ashpd::desktop::background::Background;
  use tauri::{AppHandle, Manager};

  use super::{AUTOSTART_ARG, AutostartError, failed};

  /// The last state the portal granted (the portal can't be asked).
  const STATE_FILE: &str = "autostart-portal";
  /// The command the portal's entry runs, inside the sandbox.
  const COMMAND: [&str; 2] = ["teamcraft-desktop", AUTOSTART_ARG];

  pub fn remembered(app: &AppHandle) -> bool {
    state_path(app).is_some_and(|path| fs::read_to_string(path).is_ok_and(|text| text.trim() == "on"))
  }

  /// Asks the portal; the user may refuse. What it granted is remembered.
  pub async fn request(app: &AppHandle, enabled: bool) -> Result<(), AutostartError> {
    let response = Background::request()
      .auto_start(enabled)
      .command(COMMAND)
      .reason("Start TeamCraft when you log in")
      .send()
      .await
      .and_then(|request| request.response())
      .map_err(failed)?;
    let path = state_path(app).ok_or_else(|| failed("no configuration directory"))?;
    if let Some(directory) = path.parent() {
      fs::create_dir_all(directory).map_err(failed)?;
    }
    let state = if response.auto_start() { "on" } else { "off" };
    fs::write(path, state).map_err(failed)
  }

  fn state_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join(STATE_FILE))
  }
}
