//! The global-shortcut plugin, added the first time a shortcut is set rather than at the start
//! (#1022). Its key grabber on X11 (`global-hotkey`) polls the X server every 50 ms from the moment
//! it exists, whether a key is grabbed or not: 20 wakeups a second, all day, minimized or not. Most
//! sessions never set a shortcut, and on Wayland the GlobalShortcuts portal binds the keys
//! instead (`shortcut_portal.rs`), so there the grabber is never needed.

use std::sync::{Mutex, PoisonError};

use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_global_shortcut::GlobalShortcut;

/// Two callers asking at once (push-to-talk and the mute shortcut) add the plugin once.
static ADDING: Mutex<()> = Mutex::new(());

/// The plugin, added now if `is_needed` (a shortcut is about to be registered). `None` when it
/// isn't there and isn't needed (nothing to unregister either), or could not be added.
pub fn shortcuts(app: &AppHandle, is_needed: bool) -> Option<&GlobalShortcut<Wry>> {
  if let Some(state) = app.try_state::<GlobalShortcut<Wry>>() {
    return Some(state.inner());
  }
  if !is_needed {
    return None;
  }
  let _adding = ADDING.lock().unwrap_or_else(PoisonError::into_inner);
  if app.try_state::<GlobalShortcut<Wry>>().is_none()
    && let Err(error) = app.plugin(tauri_plugin_global_shortcut::Builder::new().build())
  {
    tracing::warn!(%error, "global shortcuts unavailable");
    return None;
  }
  app.try_state::<GlobalShortcut<Wry>>().map(|state| state.inner())
}
