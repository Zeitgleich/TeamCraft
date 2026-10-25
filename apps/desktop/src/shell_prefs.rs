//! Preferences of the desktop shell itself, which it needs before the webview has loaded: whether
//! closing the window keeps the app running in the tray, whether it starts without a window, and
//! who draws the title bar.
//! Stored as `shell.json` in the app's config directory; the UI edits them in Settings.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use teamcraft_client_api::window_chrome::TitleBar;

use crate::tray::MAIN_WINDOW;
use crate::window_chrome::{self, TITLE_BAR_ENV};

const FILE_NAME: &str = "shell.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellPrefs {
  /// Closing the window hides it; the app keeps running (and notifying) in the tray.
  pub is_close_to_tray: bool,
  /// Start with the window hidden in the tray (for "start at login").
  pub is_start_minimized: bool,
  /// TeamCraft's top bar as the title bar, or the system's ("Use the system title bar").
  pub title_bar: TitleBar,
}

impl Default for ShellPrefs {
  fn default() -> Self {
    Self {
      // Linux desktops don't all show tray icons, and without one a hidden window can't come back.
      is_close_to_tray: !cfg!(target_os = "linux"),
      is_start_minimized: false,
      title_bar: default_title_bar(),
    }
  }
}

/// A packager or admin can make the system title bar the default; the user's choice still wins.
fn default_title_bar() -> TitleBar {
  std::env::var(TITLE_BAR_ENV)
    .ok()
    .and_then(|value| TitleBar::parse(&value))
    .unwrap_or_default()
}

/// The current preferences, shared by the window events and the commands.
#[derive(Default)]
pub struct ShellPrefsState(Mutex<ShellPrefs>);

#[derive(Debug, Serialize)]
pub struct ShellPrefsError {
  kind: &'static str,
  message: String,
}

impl ShellPrefsState {
  pub fn load(app: &AppHandle) -> Self {
    let prefs = path(app)
      .and_then(|path| std::fs::read_to_string(path).ok())
      .map(|text| parse(&text))
      .unwrap_or_default();
    Self(Mutex::new(prefs))
  }

  pub fn get(&self) -> ShellPrefs {
    *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn set(&self, prefs: ShellPrefs) {
    *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = prefs;
  }
}

#[tauri::command(rename_all = "snake_case")]
pub fn shell_prefs_get(state: State<'_, ShellPrefsState>) -> ShellPrefs {
  state.get()
}

#[tauri::command(rename_all = "snake_case")]
pub fn shell_prefs_set(
  app: AppHandle,
  state: State<'_, ShellPrefsState>,
  prefs: ShellPrefs,
) -> Result<(), ShellPrefsError> {
  let previous = state.get();
  state.set(prefs);
  if prefs.title_bar != previous.title_bar
    && let Some(window) = app.get_webview_window(MAIN_WINDOW)
  {
    window_chrome::apply(&window, prefs.title_bar);
  }
  let path = path(&app).ok_or_else(|| ShellPrefsError {
    kind: "failed",
    message: "no config directory".to_owned(),
  })?;
  let text = serde_json::to_string_pretty(&prefs).map_err(|error| failed(&error))?;
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent).map_err(|error| failed(&error))?;
  }
  std::fs::write(path, text).map_err(|error| failed(&error))
}

/// Unknown or broken files fall back to the defaults, field by field where possible.
fn parse(text: &str) -> ShellPrefs {
  serde_json::from_str(text).unwrap_or_default()
}

fn path(app: &AppHandle) -> Option<PathBuf> {
  app.path().app_config_dir().ok().map(|dir| dir.join(FILE_NAME))
}

fn failed(error: &impl std::fmt::Display) -> ShellPrefsError {
  ShellPrefsError {
    kind: "failed",
    message: error.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn missing_fields_and_broken_files_use_the_defaults() {
    assert_eq!(parse("{}"), ShellPrefs::default());
    assert_eq!(parse("not json"), ShellPrefs::default());
    let prefs = parse(r#"{"is_start_minimized": true}"#);
    assert!(prefs.is_start_minimized);
    assert_eq!(prefs.is_close_to_tray, ShellPrefs::default().is_close_to_tray);
    assert_eq!(parse(r#"{"title_bar": "system"}"#).title_bar, TitleBar::System);
  }

  #[test]
  fn round_trip() {
    let prefs = ShellPrefs {
      is_close_to_tray: true,
      is_start_minimized: true,
      title_bar: TitleBar::System,
    };
    let text = serde_json::to_string(&prefs).unwrap_or_default();
    assert_eq!(parse(&text), prefs);
  }
}
