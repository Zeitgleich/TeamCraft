//! What a double-click on the custom title bar does (#174): what the user chose in the desktop's
//! settings, as a native title bar would. macOS: System Settings → Desktop & Dock → "Double-click
//! a window's title bar to" (zoom, minimize, nothing). GNOME: `action-double-click-titlebar`. KDE:
//! `TitlebarDoubleClickCommand` of `kwinrc`. Windows has no such setting (it maximizes). Anything
//! the desktop says that is not known here maximizes, which is what every title bar defaults to.

#[cfg(target_os = "linux")]
use std::path::Path;

#[cfg(target_os = "linux")]
use teamcraft_client_api::window_chrome::LinuxDesktop;

/// GNOME's window manager preferences and the key for the title bar's double-click.
#[cfg(target_os = "linux")]
const GNOME_SCHEMA: &str = "org.gnome.desktop.wm.preferences";
#[cfg(target_os = "linux")]
const GNOME_KEY: &str = "action-double-click-titlebar";
/// KWin's group in `kwinrc` and the key.
#[cfg(target_os = "linux")]
const KWIN_GROUP: &str = "[Windows]";
#[cfg(target_os = "linux")]
const KWIN_KEY: &str = "TitlebarDoubleClickCommand";

/// What a double-click on the title bar does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleClickAction {
  /// Maximize, or restore a maximized window.
  ToggleMaximize,
  Minimize,
  /// Put the window below the others (Linux).
  Lower,
  Nothing,
}

impl DoubleClickAction {
  /// macOS's `AppleActionOnDoubleClick`; unset means zoom.
  #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
  pub fn from_apple(maybe_value: Option<&str>) -> Self {
    match maybe_value.map(str::trim) {
      Some("Minimize") => Self::Minimize,
      Some("None") => Self::Nothing,
      // "Maximize" (zoom), "Fill" (macOS 15 tiling) and anything newer.
      _ => Self::ToggleMaximize,
    }
  }

  /// GNOME's `action-double-click-titlebar`.
  #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
  pub fn from_gnome(value: &str) -> Self {
    match value.trim() {
      "minimize" => Self::Minimize,
      "lower" => Self::Lower,
      "none" => Self::Nothing,
      // "toggle-maximize", its horizontal and vertical variants, and "menu"/"toggle-shade", which
      // a custom title bar has no counterpart for.
      _ => Self::ToggleMaximize,
    }
  }

  /// KWin's `TitlebarDoubleClickCommand` of `kwinrc` (its text, not its option names).
  #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
  pub fn from_kwin(value: &str) -> Self {
    match value.trim() {
      "Minimize" => Self::Minimize,
      "Lower" => Self::Lower,
      "Nothing" => Self::Nothing,
      _ => Self::ToggleMaximize,
    }
  }

  /// The setting of the desktop the app runs on.
  #[cfg(target_os = "macos")]
  pub fn current() -> Self {
    Self::from_apple(teamcraft_desktop_native::apple_action_on_double_click().as_deref())
  }

  #[cfg(target_os = "linux")]
  pub fn current(desktop: LinuxDesktop) -> Self {
    match desktop {
      LinuxDesktop::Gnome => gnome_setting().map_or(Self::ToggleMaximize, |value| Self::from_gnome(&value)),
      LinuxDesktop::Kde => kwin_setting().map_or(Self::ToggleMaximize, |value| Self::from_kwin(&value)),
      LinuxDesktop::Other => Self::ToggleMaximize,
    }
  }

  #[cfg(windows)]
  pub fn current() -> Self {
    Self::ToggleMaximize
  }
}

/// GNOME's setting, when its schema is installed (a GNOME session has it).
#[cfg(target_os = "linux")]
fn gnome_setting() -> Option<String> {
  use gtk::gio::prelude::SettingsExt;

  // Looked up first: `Settings::new` aborts the process for a schema that is not installed.
  let schema = gtk::gio::SettingsSchemaSource::default()?.lookup(GNOME_SCHEMA, true)?;
  schema.has_key(GNOME_KEY).then_some(())?;
  let settings = gtk::gio::Settings::new_full(&schema, None::<&gtk::gio::SettingsBackend>, None);
  Some(settings.string(GNOME_KEY).to_string())
}

/// KWin's setting from `kwinrc`, when it is readable (not inside the Flatpak).
#[cfg(target_os = "linux")]
fn kwin_setting() -> Option<String> {
  let path = crate::window_chrome::kwinrc_path()?;
  kwinrc_value(&std::fs::read_to_string(Path::new(&path)).ok()?)
}

/// The double-click command in the text of a `kwinrc`.
#[cfg(target_os = "linux")]
fn kwinrc_value(text: &str) -> Option<String> {
  let mut is_in_group = false;
  for line in text.lines().map(str::trim) {
    if line.starts_with('[') {
      is_in_group = line == KWIN_GROUP;
    } else if is_in_group
      && let Some((key, value)) = line.split_once('=')
      && key.trim() == KWIN_KEY
    {
      return Some(value.trim().to_owned());
    }
  }
  None
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn macos_zooms_unless_told_otherwise() {
    assert_eq!(
      DoubleClickAction::from_apple(None),
      DoubleClickAction::ToggleMaximize
    );
    assert_eq!(
      DoubleClickAction::from_apple(Some("Maximize")),
      DoubleClickAction::ToggleMaximize
    );
    assert_eq!(
      DoubleClickAction::from_apple(Some("Fill")),
      DoubleClickAction::ToggleMaximize
    );
    assert_eq!(
      DoubleClickAction::from_apple(Some("Minimize")),
      DoubleClickAction::Minimize
    );
    assert_eq!(
      DoubleClickAction::from_apple(Some("None")),
      DoubleClickAction::Nothing
    );
  }

  #[test]
  fn gnome_names_map_to_actions() {
    assert_eq!(
      DoubleClickAction::from_gnome("toggle-maximize"),
      DoubleClickAction::ToggleMaximize
    );
    assert_eq!(
      DoubleClickAction::from_gnome("minimize"),
      DoubleClickAction::Minimize
    );
    assert_eq!(DoubleClickAction::from_gnome("lower"), DoubleClickAction::Lower);
    assert_eq!(DoubleClickAction::from_gnome("none"), DoubleClickAction::Nothing);
    assert_eq!(
      DoubleClickAction::from_gnome("menu"),
      DoubleClickAction::ToggleMaximize
    );
  }

  #[test]
  fn kwin_commands_map_to_actions() {
    assert_eq!(
      DoubleClickAction::from_kwin("Maximize"),
      DoubleClickAction::ToggleMaximize
    );
    assert_eq!(
      DoubleClickAction::from_kwin("Minimize"),
      DoubleClickAction::Minimize
    );
    assert_eq!(DoubleClickAction::from_kwin("Lower"), DoubleClickAction::Lower);
    assert_eq!(
      DoubleClickAction::from_kwin("Nothing"),
      DoubleClickAction::Nothing
    );
    assert_eq!(
      DoubleClickAction::from_kwin("Shade"),
      DoubleClickAction::ToggleMaximize
    );
  }

  #[cfg(target_os = "linux")]
  #[test]
  fn kwinrc_value_is_read_from_the_windows_group_only() {
    let text = "[Other]\nTitlebarDoubleClickCommand=Nothing\n[Windows]\nBorderSnapZone=10\nTitlebarDoubleClickCommand = Lower\n";
    assert_eq!(kwinrc_value(text).as_deref(), Some("Lower"));
    assert_eq!(kwinrc_value("[Windows]\nFoo=1\n"), None);
    assert_eq!(
      kwinrc_value("[Other]\nTitlebarDoubleClickCommand=Nothing\n"),
      None
    );
  }
}
