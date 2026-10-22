//! Push-to-talk on the desktop: a global shortcut opens the microphone while it is held.
//!
//! Global shortcuts need the desktop's cooperation: they work on Windows, macOS and X11; on
//! Wayland registration fails, and the UI falls back to the key inside its window
//! (`CallAction::SetTalking`), as in the browser.

use std::sync::Arc;

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tracing::warn;

use super::CallHost;

/// Unregisters `maybe_previous` and registers `maybe_shortcut`; `true` if the new shortcut is
/// active globally.
pub(super) fn register(
  app: &AppHandle,
  calls: &Arc<CallHost>,
  maybe_previous: Option<&str>,
  maybe_shortcut: Option<&str>,
) -> bool {
  let shortcuts = app.global_shortcut();
  if let Some(previous) = maybe_previous
    && let Err(error) = shortcuts.unregister(previous)
  {
    // Never registered (e.g. on Wayland) or already gone; nothing to undo.
    tracing::debug!(%error, "push-to-talk shortcut not unregistered");
  }
  let Some(shortcut) = maybe_shortcut else {
    return false;
  };
  let calls = Arc::clone(calls);
  let result = shortcuts.on_shortcut(shortcut, move |_app, _shortcut, event| {
    let calls = Arc::clone(&calls);
    let is_talking = event.state == ShortcutState::Pressed;
    tauri::async_runtime::spawn(async move { calls.set_talking(is_talking).await });
  });
  match result {
    Ok(()) => true,
    Err(error) => {
      warn!(%error, shortcut, "global push-to-talk shortcut unavailable; the window handles the key");
      false
    },
  }
}
