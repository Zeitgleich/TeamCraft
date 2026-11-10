//! The global mute shortcut: while a call runs, the key mutes and unmutes the microphone in every
//! app. The UI decides when it is wanted (setting on, a call active) and asks for the shortcut with
//! `mute_shortcut_set`; a press goes back to the main window as the `toggle_mute` tray action, which
//! does what the microphone button does. The window is not brought up.
//!
//! On Wayland apps can't grab keys: the GlobalShortcuts portal binds the key instead, as for
//! push-to-talk (`shortcut_portal.rs`), with a session of its own that is held during calls only.
//! Elsewhere (X11, macOS, Windows) the global-shortcut plugin grabs it.

use std::str::FromStr;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Shortcut, ShortcutState};
use teamcraft_client_api::tray_menu::{MuteShortcutStatus, TRAY_ACTION_EVENT, TrayAction};

use crate::tray::MAIN_WINDOW;

/// The shortcut that is registered right now.
#[derive(Default)]
pub struct MuteShortcutState {
  maybe_registered: Mutex<Option<Shortcut>>,
  /// Wayland: the accelerator bound through the portal, and its session. Async, as binding may
  /// wait for the user in the desktop's dialog; it also keeps the UI's requests in order.
  #[cfg(target_os = "linux")]
  maybe_portal: tokio::sync::Mutex<Option<(String, crate::shortcut_portal::Binding)>>,
}

/// Parses a Tauri accelerator such as `CommandOrControl+Shift+M`.
fn parse(accelerator: &str) -> Option<Shortcut> {
  Shortcut::from_str(accelerator.trim()).ok()
}

/// Hands a press to the main window.
fn toggle_mute(app: &AppHandle) {
  if let Err(error) = app.emit_to(MAIN_WINDOW, TRAY_ACTION_EVENT, TrayAction::ToggleMute.id()) {
    tracing::debug!(%error, "could not hand the mute shortcut to the window");
  }
}

/// Registers `maybe_shortcut` (a Tauri accelerator), or drops the one registered before. With
/// `is_active` false the shortcut is only tried, to tell the settings whether it can be had, and
/// released again at once (on Wayland: whether the desktop has the portal, without asking the
/// user anything). Without any shortcut support or a tray: it still only answers.
#[tauri::command(rename_all = "snake_case")]
pub async fn mute_shortcut_set(
  app: AppHandle,
  maybe_shortcut: Option<String>,
  is_active: bool,
) -> MuteShortcutStatus {
  #[cfg(target_os = "linux")]
  if crate::shortcut_portal::is_wayland() {
    return set_with_portal(&app, maybe_shortcut, is_active).await;
  }
  set_with_plugin(&app, maybe_shortcut, is_active)
}

fn set_with_plugin(app: &AppHandle, maybe_shortcut: Option<String>, is_active: bool) -> MuteShortcutStatus {
  let state = app.state::<MuteShortcutState>();
  let Some(shortcuts) = crate::global_keys::shortcuts(app, maybe_shortcut.is_some()) else {
    return match maybe_shortcut {
      None => MuteShortcutStatus::Off,
      Some(_) => MuteShortcutStatus::Taken,
    };
  };
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
  let result = shortcuts.on_shortcut(shortcut, |app, _shortcut, event| {
    if event.state == ShortcutState::Pressed {
      toggle_mute(app);
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

/// Wayland: binds the key through the portal while a call runs and closes the session after it,
/// so the key is free again. The same key in the next request keeps the session it has.
#[cfg(target_os = "linux")]
async fn set_with_portal(
  app: &AppHandle,
  maybe_shortcut: Option<String>,
  is_active: bool,
) -> MuteShortcutStatus {
  use crate::shortcut_portal::{self, xdg_trigger};

  const SHORTCUT_ID: &str = "toggle-mute";
  /// Shown by the desktop's shortcut dialog, in the app's source language (the portal has no
  /// translation hook).
  const SHORTCUT_DESCRIPTION: &str = "Mute or unmute the microphone in a call";

  let state = app.state::<MuteShortcutState>();
  let mut bound = state.maybe_portal.lock().await;
  let Some(accelerator) = maybe_shortcut else {
    drop(bound.take());
    return MuteShortcutStatus::Off;
  };
  if parse(&accelerator).is_none() {
    drop(bound.take());
    return MuteShortcutStatus::Invalid;
  }
  if !is_active {
    drop(bound.take());
    return match shortcut_portal::is_available().await {
      true => MuteShortcutStatus::Ready,
      false => MuteShortcutStatus::Unavailable,
    };
  }
  if bound.as_ref().is_some_and(|(current, _)| *current == accelerator) {
    return MuteShortcutStatus::Ready;
  }
  // The previous session closes before a new one binds the same id.
  drop(bound.take());
  let handle = app.clone();
  let on_event = move |is_pressed: bool| {
    if is_pressed {
      toggle_mute(&handle);
    }
    std::future::ready(())
  };
  match shortcut_portal::bind(
    SHORTCUT_ID,
    SHORTCUT_DESCRIPTION,
    xdg_trigger(&accelerator),
    on_event,
  )
  .await
  {
    Ok(binding) => {
      *bound = Some((accelerator, binding));
      MuteShortcutStatus::Ready
    },
    Err(error) => {
      tracing::warn!(%error, "mute shortcut not bound through the portal");
      MuteShortcutStatus::Unavailable
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
