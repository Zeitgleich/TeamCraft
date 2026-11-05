//! Push-to-talk on the desktop: a global shortcut opens the microphone while it is held.
//!
//! On Wayland, apps can't grab keys; the GlobalShortcuts portal (GNOME 48+, KDE Plasma 5.27+,
//! also inside the Flatpak sandbox) binds the shortcut instead: the desktop asks the user once
//! and then reports presses and releases. Elsewhere (X11, macOS) the global-shortcut plugin grabs
//! the key. Where neither works, the UI falls back to the key inside its window
//! (`CallAction::SetTalking`), as in the browser.

use std::sync::Arc;

use tauri::AppHandle;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};
use tracing::warn;

use super::CallHost;

/// Unregisters `maybe_previous` and registers `maybe_shortcut` (a Tauri accelerator such as
/// `CommandOrControl+Shift+Space`); `true` if the new shortcut is active globally.
pub(super) async fn register(
  app: &AppHandle,
  calls: &Arc<CallHost>,
  maybe_previous: Option<&str>,
  maybe_shortcut: Option<&str>,
) -> bool {
  #[cfg(target_os = "linux")]
  if portal::is_wayland() {
    return portal::register(app, calls, maybe_shortcut).await;
  }
  register_with_plugin(app, calls, maybe_previous, maybe_shortcut)
}

/// Push-to-talk state the app keeps (`app.manage`): the portal binding on Wayland.
#[derive(Default)]
pub struct PushToTalkState {
  #[cfg(target_os = "linux")]
  binding: tokio::sync::Mutex<Option<portal::Binding>>,
}

fn register_with_plugin(
  app: &AppHandle,
  calls: &Arc<CallHost>,
  maybe_previous: Option<&str>,
  maybe_shortcut: Option<&str>,
) -> bool {
  let shortcuts = app.global_shortcut();
  if let Some(previous) = maybe_previous
    && let Err(error) = shortcuts.unregister(previous)
  {
    // Never registered or already gone; nothing to undo.
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

#[cfg(target_os = "linux")]
mod portal {
  use std::sync::Arc;

  use tauri::{AppHandle, Manager};
  use tracing::warn;

  use super::{CallHost, PushToTalkState};
  use crate::shortcut_portal::{self, xdg_trigger};

  const SHORTCUT_ID: &str = "push-to-talk";
  /// Shown by the desktop's shortcut dialog; the portal has no translation hook, so the app's
  /// source language is used.
  const SHORTCUT_DESCRIPTION: &str = "Push to talk";

  /// The bound portal shortcut; dropping it closes the session.
  pub(super) type Binding = shortcut_portal::Binding;

  pub(super) fn is_wayland() -> bool {
    shortcut_portal::is_wayland()
  }

  pub(super) async fn register(app: &AppHandle, calls: &Arc<CallHost>, maybe_shortcut: Option<&str>) -> bool {
    let state = app.state::<PushToTalkState>();
    let mut binding = state.binding.lock().await;
    // The previous session closes before a new one binds the same id.
    drop(binding.take());
    let Some(shortcut) = maybe_shortcut else {
      return false;
    };
    let calls = Arc::clone(calls);
    let on_event = move |is_talking: bool| {
      let calls = Arc::clone(&calls);
      async move { calls.set_talking(is_talking).await }
    };
    match shortcut_portal::bind(SHORTCUT_ID, SHORTCUT_DESCRIPTION, xdg_trigger(shortcut), on_event).await {
      Ok(new) => {
        *binding = Some(new);
        true
      },
      Err(error) => {
        warn!(%error, "GlobalShortcuts portal unavailable; the window handles the push-to-talk key");
        false
      },
    }
  }
}
