//! Global shortcuts on Wayland, where apps can't grab keys: the GlobalShortcuts portal (GNOME 48+,
//! KDE Plasma 5.27+, also inside the Flatpak sandbox) binds a shortcut instead. The desktop asks
//! the user once (and may let them pick another key), then reports presses and releases. Used by
//! push-to-talk (`calls/push_to_talk.rs`) and the mute shortcut (`mute_shortcut.rs`), each with
//! its own session, so one can be bound and released without the other.

/// A Tauri accelerator as an XDG shortcuts trigger (`CommandOrControl+Shift+Space` →
/// `CTRL+SHIFT+space`), the portal's preferred trigger. `None` for keys it can't name.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn xdg_trigger(accelerator: &str) -> Option<String> {
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
pub use portal::{Binding, bind, is_available, is_wayland};

#[cfg(target_os = "linux")]
mod portal {
  use std::future::Future;

  use ashpd::desktop::Session;
  use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
  use futures_util::StreamExt;
  use tokio::sync::oneshot;
  use tracing::{info, warn};

  /// A bound portal shortcut; dropping it closes the session, which frees the key again.
  pub struct Binding {
    /// Held only: dropping the sender ends the listener.
    _stop: oneshot::Sender<()>,
  }

  pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
  }

  /// Whether the desktop offers the portal at all; asks nothing of the user.
  pub async fn is_available() -> bool {
    match GlobalShortcuts::new().await {
      Ok(_) => true,
      Err(error) => {
        tracing::debug!(%error, "no GlobalShortcuts portal");
        false
      },
    }
  }

  /// Creates a session, asks the desktop to bind the shortcut `id` (it may show a dialog with
  /// `description`, which the portal can't translate) and calls `on_event` with `true` on a press
  /// and `false` on a release, in order, until the binding is dropped.
  pub async fn bind<F, Fut>(
    id: &'static str,
    description: &'static str,
    maybe_trigger: Option<String>,
    on_event: F,
  ) -> Result<Binding, ashpd::Error>
  where
    F: Fn(bool) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let portal = GlobalShortcuts::new().await?;
    let session = portal.create_session(Default::default()).await?;
    let shortcut = NewShortcut::new(id, description).preferred_trigger(maybe_trigger.as_deref());
    let bound = portal
      .bind_shortcuts(&session, &[shortcut], None, Default::default())
      .await?
      .response()?;
    match bound.shortcuts().iter().find(|shortcut| shortcut.id() == id) {
      Some(shortcut) => info!(
        id,
        trigger = shortcut.trigger_description(),
        "shortcut bound through the portal"
      ),
      None => {
        close(&session).await;
        return Err(ashpd::Error::NoResponse);
      },
    }
    let (stop, stopped) = oneshot::channel();
    tauri::async_runtime::spawn(listen(portal, session, id, on_event, stopped));
    Ok(Binding { _stop: stop })
  }

  async fn listen<F, Fut>(
    portal: GlobalShortcuts,
    session: Session<GlobalShortcuts>,
    id: &'static str,
    on_event: F,
    mut stopped: oneshot::Receiver<()>,
  ) where
    F: Fn(bool) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
  {
    let (activated, deactivated) = match (
      portal.receive_activated().await,
      portal.receive_deactivated().await,
    ) {
      (Ok(activated), Ok(deactivated)) => (activated, deactivated),
      (Err(error), _) | (_, Err(error)) => {
        warn!(%error, id, "shortcut events unavailable");
        close(&session).await;
        return;
      },
    };
    let mut activated = std::pin::pin!(activated);
    let mut deactivated = std::pin::pin!(deactivated);
    // The signals come for every session of the app; the id tells ours apart.
    let ours = |shortcut_id: &str| shortcut_id == id;
    loop {
      tokio::select! {
        _ = &mut stopped => break,
        Some(event) = activated.next() => {
          if ours(event.shortcut_id()) {
            on_event(true).await;
          }
        },
        Some(event) = deactivated.next() => {
          if ours(event.shortcut_id()) {
            on_event(false).await;
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
      tracing::debug!(%error, "shortcut session not closed");
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
