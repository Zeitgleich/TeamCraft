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

/// A Tauri accelerator as an XDG shortcuts trigger (`CommandOrControl+Shift+Space` →
/// `CTRL+SHIFT+space`), the portal's preferred trigger. `None` for keys it can't name.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn xdg_trigger(accelerator: &str) -> Option<String> {
  let mut parts = Vec::new();
  for part in accelerator.split('+').map(str::trim) {
    let modifier = match part.to_ascii_lowercase().as_str() {
      "commandorcontrol" | "cmdorctrl" | "control" | "ctrl" => Some("CTRL"),
      "shift" => Some("SHIFT"),
      "alt" | "option" => Some("ALT"),
      "super" | "meta" | "command" | "cmd" => Some("LOGO"),
      _ => None,
    };
    if let Some(modifier) = modifier {
      parts.push(modifier.to_owned());
      continue;
    }
    parts.push(xdg_key(part)?);
  }
  Some(parts.join("+"))
}

/// A key as its xkb keysym name.
fn xdg_key(key: &str) -> Option<String> {
  let lower = key.to_ascii_lowercase();
  let name = match lower.as_str() {
    "space" => "space".to_owned(),
    "enter" | "return" => "Return".to_owned(),
    "tab" => "Tab".to_owned(),
    "backquote" => "grave".to_owned(),
    _ if lower.len() == 1 && lower.chars().all(|c| c.is_ascii_alphanumeric()) => lower,
    _ if lower.starts_with('f') && lower[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
      key.to_ascii_uppercase()
    },
    _ => return None,
  };
  Some(name)
}

#[cfg(target_os = "linux")]
mod portal {
  use std::sync::Arc;

  use ashpd::desktop::Session;
  use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
  use futures_util::StreamExt;
  use tauri::{AppHandle, Manager};
  use tokio::sync::oneshot;
  use tracing::{info, warn};

  use super::{CallHost, PushToTalkState, xdg_trigger};

  const SHORTCUT_ID: &str = "push-to-talk";
  /// Shown by the desktop's shortcut dialog; the portal has no translation hook, so the app's
  /// source language is used.
  const SHORTCUT_DESCRIPTION: &str = "Push to talk";

  /// A bound portal shortcut; dropping `stop` (or sending on it) closes the session.
  pub(super) struct Binding {
    stop: oneshot::Sender<()>,
  }

  pub(super) fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
  }

  pub(super) async fn register(app: &AppHandle, calls: &Arc<CallHost>, maybe_shortcut: Option<&str>) -> bool {
    let state = app.state::<PushToTalkState>();
    let mut binding = state.binding.lock().await;
    if let Some(previous) = binding.take() {
      // The listener ends either way: it also stops when the sender is dropped.
      let _ = previous.stop.send(());
    }
    let Some(shortcut) = maybe_shortcut else {
      return false;
    };
    match bind(Arc::clone(calls), xdg_trigger(shortcut)).await {
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

  /// Creates a session, asks the desktop to bind the shortcut (it may show a dialog and let the
  /// user pick another key) and listens for presses and releases until stopped.
  async fn bind(calls: Arc<CallHost>, maybe_trigger: Option<String>) -> Result<Binding, ashpd::Error> {
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session(Default::default()).await?;
    let shortcut =
      NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION).preferred_trigger(maybe_trigger.as_deref());
    let bound = portal
      .bind_shortcuts(&session, &[shortcut], None, Default::default())
      .await?
      .response()?;
    match bound
      .shortcuts()
      .iter()
      .find(|shortcut| shortcut.id() == SHORTCUT_ID)
    {
      Some(shortcut) => info!(
        trigger = shortcut.trigger_description(),
        "push-to-talk bound through the portal"
      ),
      None => {
        close(&session).await;
        return Err(ashpd::Error::NoResponse);
      },
    }
    let (stop, stopped) = oneshot::channel();
    tauri::async_runtime::spawn(listen(portal, session, calls, stopped));
    Ok(Binding { stop })
  }

  async fn listen(
    portal: GlobalShortcuts,
    session: Session<GlobalShortcuts>,
    calls: Arc<CallHost>,
    mut stopped: oneshot::Receiver<()>,
  ) {
    let (activated, deactivated) = match (
      portal.receive_activated().await,
      portal.receive_deactivated().await,
    ) {
      (Ok(activated), Ok(deactivated)) => (activated, deactivated),
      (Err(error), _) | (_, Err(error)) => {
        warn!(%error, "push-to-talk events unavailable");
        close(&session).await;
        return;
      },
    };
    let mut activated = std::pin::pin!(activated);
    let mut deactivated = std::pin::pin!(deactivated);
    // One binding at a time (the previous session is closed first), so the id identifies ours.
    let ours = |id: &str| id == SHORTCUT_ID;
    loop {
      tokio::select! {
        _ = &mut stopped => break,
        Some(event) = activated.next() => {
          if ours(event.shortcut_id()) {
            calls.set_talking(true).await;
          }
        },
        Some(event) = deactivated.next() => {
          if ours(event.shortcut_id()) {
            calls.set_talking(false).await;
          }
        },
        else => break,
      }
    }
    close(&session).await;
  }

  async fn close(session: &Session<GlobalShortcuts>) {
    if let Err(error) = session.close().await {
      // The portal drops the session with our connection anyway.
      tracing::debug!(%error, "push-to-talk session not closed");
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tauri_accelerators_become_xdg_triggers() {
    assert_eq!(
      xdg_trigger("CommandOrControl+Shift+Space").as_deref(),
      Some("CTRL+SHIFT+space")
    );
    assert_eq!(xdg_trigger("F8").as_deref(), Some("F8"));
    assert_eq!(xdg_trigger("Alt+T").as_deref(), Some("ALT+t"));
    assert_eq!(xdg_trigger("Super+Backquote").as_deref(), Some("LOGO+grave"));
    assert_eq!(xdg_trigger("Ctrl+MediaPlayPause"), None);
  }
}
