//! The ringtone of an incoming call, on the default output (notifications.md, "Calls"). The webview
//! decides when a call rings audibly (the user's setting, mutes, the app's do-not-disturb); the
//! shell adds the desktop's own do-not-disturb where it can read it (Linux) and plays
//! `teamcraft-media`'s synthesized ringtone until told to stop.

use std::sync::Mutex;

use tauri::State;
use teamcraft_media::audio_io::AudioDeviceChoice;
use teamcraft_media::ringtone::RingtonePlayer;

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
) -> Result<(), ()> {
  let is_quiet = is_ringing && is_desktop_quiet(&probe).await;
  if is_quiet {
    tracing::debug!("the desktop's do-not-disturb is on: no ringtone");
  }
  let maybe_player = (is_ringing && !is_quiet).then(|| RingtonePlayer::start(AudioDeviceChoice::Default));
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

/// Whether the desktop asked for quiet: GNOME's or KDE's do-not-disturb. macOS Focus and Windows'
/// do-not-disturb are not readable by apps without extra entitlements; there the OS silences the
/// notification and the in-app setting silences the ringtone. The message sound asks the same.
pub(crate) async fn is_desktop_quiet(probe: &crate::attention::OsProbe) -> bool {
  #[cfg(target_os = "linux")]
  {
    probe.buses().is_do_not_disturb().await
  }
  #[cfg(not(target_os = "linux"))]
  {
    let _ = probe;
    false
  }
}
