//! The global mute shortcut: while a call runs, the key mutes and unmutes the microphone in every
//! app. The UI decides when it is wanted (setting on, a call active) and asks for the shortcut with
//! `mute_shortcut_set`; a press goes back to the main window as the `toggle_mute` tray action, which
//! does what the microphone button does. The window is not brought up.
//!
//! On Wayland apps can't grab keys, so the shortcut isn't offered there (the key stays free).

use std::str::FromStr;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, State};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use teamcraft_client_api::tray_menu::{MuteShortcutStatus, TRAY_ACTION_EVENT, TrayAction};

use crate::tray::MAIN_WINDOW;

/// The shortcut that is registered right now.
#[derive(Default)]
pub struct MuteShortcutState {
  maybe_registered: Mutex<Option<Shortcut>>,
}

/// Whether this desktop lets an app grab keys at all.
fn can_grab_keys() -> bool {
  !(cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some())
}

/// Parses a Tauri accelerator such as `CommandOrControl+Shift+M`.
fn parse(accelerator: &str) -> Option<Shortcut> {
  Shortcut::from_str(accelerator.trim()).ok()
}

/// Registers `maybe_shortcut` (a Tauri accelerator), or drops the one registered before. With
/// `is_active` false the shortcut is only tried, to tell the settings whether it can be had, and
/// released again at once. Without any shortcut support or a tray: it still only answers.
#[tauri::command(rename_all = "snake_case")]
pub fn mute_shortcut_set(
  app: AppHandle,
  state: State<'_, MuteShortcutState>,
  maybe_shortcut: Option<String>,
  is_active: bool,
) -> MuteShortcutStatus {
  let shortcuts = app.global_shortcut();
  let mut registered = state
    .maybe_registered
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner);
  if let Some(previous) = registered.take()
    && let Err(error) = shortcuts.unregister(previous)
  {
    tracing::debug!(%error, "mute shortcut not unregistered");
  }
  let Some(accelerator) = maybe_shortcut else {
    return MuteShortcutStatus::Off;
  };
  let Some(shortcut) = parse(&accelerator) else {
    return MuteShortcutStatus::Invalid;
  };
  if !can_grab_keys() {
    return MuteShortcutStatus::Unavailable;
  }
  let result = shortcuts.on_shortcut(shortcut, |app, _shortcut, event| {
    if event.state == ShortcutState::Pressed
      && let Err(error) = app.emit_to(MAIN_WINDOW, TRAY_ACTION_EVENT, TrayAction::ToggleMute.id())
    {
      tracing::debug!(%error, "could not hand the mute shortcut to the window");
    }
  });
  match result {
    Ok(()) if is_active => {
      *registered = Some(shortcut);
      MuteShortcutStatus::Ready
    },
    Ok(()) => {
      if let Err(error) = shortcuts.unregister(shortcut) {
        tracing::debug!(%error, "mute shortcut probe not released");
      }
      MuteShortcutStatus::Ready
    },
    Err(error) => {
      tracing::debug!(%error, "mute shortcut taken");
      MuteShortcutStatus::Taken
    },
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn accelerators_the_settings_write_parse() {
    for accelerator in ["CommandOrControl+Shift+M", "Alt+F9", "Control+Alt+Space", "F13"] {
      assert!(parse(accelerator).is_some(), "{accelerator}");
    }
  }

  #[test]
  fn nonsense_is_not_a_shortcut() {
    for accelerator in ["", "Shift+", "Ctrl+Nonsense", "+++"] {
      assert!(parse(accelerator).is_none(), "{accelerator}");
    }
  }
}
