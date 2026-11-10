//! The ringtone of an incoming call, on the speakers chosen for calls (notifications.md, "Calls"). The webview
//! decides when a call rings audibly (the user's setting, mutes, the app's do-not-disturb); the
//! shell adds the desktop's own do-not-disturb where it can read it (Linux) and plays
//! `teamcraft-media`'s synthesized ringtone until told to stop.

use std::sync::Mutex;

use tauri::State;
use teamcraft_media::ringtone::{FULL_VOLUME_PERCENT, RingtonePlayer};

/// The ringtone while it plays.
#[derive(Default)]
pub struct RingtoneState {
  maybe_player: Mutex<Option<RingtonePlayer>>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn set_ringtone(
  ringtone: State<'_, RingtoneState>,
  probe: State<'_, crate::attention::OsProbe>,
  is_ringing: bool,
  maybe_speakers: Option<String>,
  volume_percent: Option<u8>,
) -> Result<(), ()> {
  let is_quiet = is_ringing && is_desktop_quiet(&probe).await;
  if is_quiet {
    tracing::debug!("the desktop's do-not-disturb is on: no ringtone");
  }
  let maybe_player = (is_ringing && !is_quiet).then(|| {
    RingtonePlayer::start(
      crate::calls::device_choice(maybe_speakers.filter(|id| !id.is_empty())),
      volume_percent.unwrap_or(FULL_VOLUME_PERCENT),
    )
  });
  let previous = std::mem::replace(
    &mut *ringtone
      .maybe_player
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner),
    maybe_player,
  );
  // Stopping joins the player's thread (a few milliseconds); not under the lock.
  drop(previous);
  Ok(())
}

/// Whether the desktop asked for quiet: GNOME's or KDE's do-not-disturb, and on Windows the
/// shell's documented `SHQueryUserNotificationState` (presentation mode, a full-screen app, quiet
/// time; Focus assist is not reported by a documented API). macOS Focus is not readable without
/// the Communication Notifications entitlement; there the OS silences the notification and the
/// in-app setting silences the ringtone. The message sound asks the same.
pub(crate) async fn is_desktop_quiet(probe: &crate::attention::OsProbe) -> bool {
  #[cfg(target_os = "linux")]
  {
    probe.buses().is_do_not_disturb().await
  }
  #[cfg(windows)]
  {
    let _ = probe;
    windows_state_is_quiet()
  }
  #[cfg(not(any(target_os = "linux", windows)))]
  {
    let _ = probe;
    false
  }
}

/// Whether a `QUERY_USER_NOTIFICATION_STATE` asks apps not to show notifications: `QUNS_BUSY`
/// (2), `QUNS_RUNNING_D3D_FULL_SCREEN` (3), `QUNS_PRESENTATION_MODE` (4) and `QUNS_QUIET_TIME` (6).
/// Not present (1), accepts notifications (5) and a Windows Store app (7) are not quiet.
#[cfg_attr(not(windows), allow(dead_code))]
fn notification_state_is_quiet(state: i32) -> bool {
  matches!(state, 2..=4 | 6)
}

#[cfg(windows)]
fn windows_state_is_quiet() -> bool {
  // SAFETY: takes no arguments and only reads the shell's state.
  unsafe { windows::Win32::UI::Shell::SHQueryUserNotificationState() }
    .is_ok_and(|state| notification_state_is_quiet(state.0))
}

#[cfg(test)]
mod tests {
  use super::notification_state_is_quiet;

  #[test]
  fn windows_notification_states_that_ask_for_quiet() {
    let quiet: Vec<i32> = (0..=8)
      .filter(|state| notification_state_is_quiet(*state))
      .collect();
    assert_eq!(quiet, [2, 3, 4, 6]);
  }
}
