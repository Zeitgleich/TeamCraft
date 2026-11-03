//! The sound for new messages on desktop and web (notifications.md, "Message sounds"; #800).
//!
//! The notifier decides which messages notify (levels, mutes, do-not-disturb); for those this
//! plays the user's choice, once for a burst ([`SoundThrottle`]), and tells the notification
//! whether the system should play its own sound as well ([`notification_sound`]). The desktop
//! shell plays on the default output and keeps the own file in the app's data directory
//! (`apps/desktop/src/message_sound.rs`); in a browser the page plays through Web Audio and keeps
//! the file in IndexedDB ([`page`]). Either way the file stays on this device, and a sound that
//! can't play never holds up a notification: a missing or broken own file plays the default sound
//! and the settings say why.

use leptos::prelude::*;
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_client_api::message_sound::{
  MessageSound, OwnSoundLimits, OwnSoundPick, OwnSoundStatus, SoundPlayed,
};
use teamcraft_client_api::os_notify::NotificationSound;

use crate::auth::tauri;
use crate::clock::now_ms;
use crate::prefs::NotificationPrefs;
use crate::reactive::SetIfAlive;

#[cfg(target_arch = "wasm32")]
mod page;

/// Messages within this time of the last sound are one burst: they play no sound of their own.
const BURST_GAP_MS: i64 = 2_000;

/// The message sounds of the signed-in app, provided as context by `os_alerts::start`.
#[derive(Clone, Copy)]
pub struct MessageSounds {
  /// What the settings show about the own file; `None` until it was looked at.
  pub own_status: RwSignal<Option<OwnSoundStatus>>,
  throttle: StoredValue<SoundThrottle, LocalStorage>,
}

/// One sound for a burst of messages: a sound plays only when the last one is [`BURST_GAP_MS`]
/// ago. Measured from the last sound, so a steady stream plays every two seconds at most.
#[derive(Debug, Default)]
pub struct SoundThrottle {
  maybe_last_ms: Option<i64>,
}

impl MessageSounds {
  pub fn provide() -> Self {
    let sounds = Self {
      own_status: RwSignal::new(None),
      throttle: StoredValue::new_local(SoundThrottle::default()),
    };
    provide_context(sounds);
    sounds
  }

  /// Messages that notify arrived: plays the chosen sound, once for a burst. Not during a call (as
  /// the ringtone: the call has the speakers), and not where the page may not play sound.
  pub fn on_notifications(self, prefs: &NotificationPrefs, is_in_call: bool) {
    let sound = prefs.message_sound;
    if !sound.is_played_by_app() || is_in_call || !can_play() {
      return;
    }
    let may_play = self
      .throttle
      .try_update_value(|throttle| throttle.may_play(now_ms()))
      .unwrap_or(false);
    if may_play {
      self.play(sound, prefs.own_sound_limits, false);
    }
  }

  /// The settings' preview: plays `sound` now, whatever the throttle and the desktop's
  /// do-not-disturb say. Call it in the click (or change) handler: a browser lets a page start
  /// sound only there.
  pub fn preview(self, sound: MessageSound, limits: OwnSoundLimits) {
    self.play(sound, limits, true);
  }

  /// Desktop: "Own file…" opens the system's dialog; a file that passes the checks plays from now
  /// on. `filter_label` names the dialog's filter.
  pub async fn pick_in_shell(
    self,
    limits: OwnSoundLimits,
    filter_label: String,
  ) -> Result<OwnSoundPick, ActionFailure> {
    let args = serde_json::json!({ "limits": limits, "filter_label": filter_label });
    let pick = tauri::call::<OwnSoundPick>("message_sound_pick", &args)
      .await
      .map_err(ActionFailure::other)?;
    self.after_pick(&pick);
    Ok(pick)
  }

  /// Browser: keeps a file picked in the page, once it decodes.
  #[cfg(target_arch = "wasm32")]
  pub async fn keep_picked(
    self,
    file: web_sys::File,
    limits: OwnSoundLimits,
  ) -> Result<OwnSoundPick, ActionFailure> {
    let pick = page::keep(file, limits).await.map_err(ActionFailure::other)?;
    self.after_pick(&pick);
    Ok(pick)
  }

  /// Looks at the own file again (the settings opened, or a limit changed).
  pub async fn refresh_status(self, limits: OwnSoundLimits) {
    let status = if tauri::is_available() {
      let args = serde_json::json!({ "limits": limits });
      match tauri::call::<OwnSoundStatus>("message_sound_status", &args).await {
        Ok(status) => status,
        Err(error) => {
          tracing::debug!(%error, "could not look at the own message sound");
          return;
        },
      }
    } else {
      page_status(limits).await
    };
    self.own_status.set_if_alive(Some(status));
  }

  /// Removes the own file from this device.
  pub async fn remove(self) -> Result<(), ActionFailure> {
    if tauri::is_available() {
      tauri::call::<()>("message_sound_remove", &serde_json::json!({}))
        .await
        .map_err(ActionFailure::other)?;
    } else {
      page_remove().await.map_err(ActionFailure::other)?;
    }
    self.own_status.set_if_alive(Some(OwnSoundStatus::NotPicked));
    Ok(())
  }

  fn play(self, sound: MessageSound, limits: OwnSoundLimits, is_preview: bool) {
    if tauri::is_available() {
      self.play_in_shell(sound, limits, is_preview);
    } else {
      self.play_in_page(sound, limits);
    }
  }

  /// Desktop: the shell plays it, after its own look at the desktop's do-not-disturb.
  fn play_in_shell(self, sound: MessageSound, limits: OwnSoundLimits, is_preview: bool) {
    leptos::task::spawn_local(async move {
      let args = serde_json::json!({ "sound": sound, "limits": limits, "is_preview": is_preview });
      match tauri::call::<SoundPlayed>("message_sound_play", &args).await {
        Ok(played) => self.after_play(played),
        Err(error) => tracing::debug!(%error, "could not play the message sound"),
      }
    });
  }

  /// Browser: Web Audio in this page.
  fn play_in_page(self, sound: MessageSound, limits: OwnSoundLimits) {
    #[cfg(target_arch = "wasm32")]
    {
      // Made now: inside the click for a preview, which is what lets a browser play.
      let maybe_context = page::start_context();
      leptos::task::spawn_local(async move {
        let played = page::play(maybe_context, sound, limits).await;
        self.after_play(played);
      });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (self, sound, limits);
  }

  fn after_play(self, played: SoundPlayed) {
    if played == SoundPlayed::FellBack {
      self.own_status.set_if_alive(Some(OwnSoundStatus::Unavailable));
    }
  }

  fn after_pick(self, pick: &OwnSoundPick) {
    if let OwnSoundPick::Stored(info) = pick {
      self
        .own_status
        .set_if_alive(Some(OwnSoundStatus::Ready(info.clone())));
    }
  }
}

impl SoundThrottle {
  /// Whether a sound may play at `now_ms`; if so, it counts as played.
  pub fn may_play(&mut self, now_ms: i64) -> bool {
    let is_in_burst = self
      .maybe_last_ms
      .is_some_and(|last| (0..BURST_GAP_MS).contains(&(now_ms - last)));
    if is_in_burst {
      return false;
    }
    self.maybe_last_ms = Some(now_ms);
    true
  }
}

/// Whether the system plays its own sound with a message notification.
pub fn notification_sound(sound: MessageSound) -> NotificationSound {
  notification_sound_for(sound, can_play())
}

/// [`notification_sound`] for a page that can or can't play sound: where the app's own sound can't
/// play (a browser tab nobody has touched yet), the notification keeps the system's, so a message
/// isn't silent by accident.
fn notification_sound_for(sound: MessageSound, can_play: bool) -> NotificationSound {
  if sound.is_played_by_app() && !can_play {
    NotificationSound::System
  } else {
    sound.notification_sound()
  }
}

/// The desktop shell always can; a browser tab only after the user interacted with it.
fn can_play() -> bool {
  #[cfg(target_arch = "wasm32")]
  {
    tauri::is_available() || page::may_play()
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    tauri::is_available()
  }
}

async fn page_status(limits: OwnSoundLimits) -> OwnSoundStatus {
  #[cfg(target_arch = "wasm32")]
  {
    page::status(limits).await
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    let _ = limits;
    OwnSoundStatus::NotPicked
  }
}

async fn page_remove() -> Result<(), String> {
  #[cfg(target_arch = "wasm32")]
  {
    page::remove().await
  }
  #[cfg(not(target_arch = "wasm32"))]
  {
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::message_sound::BuiltInSound;

  use super::*;

  #[test]
  fn a_burst_of_messages_plays_one_sound() {
    let mut throttle = SoundThrottle::default();
    assert!(throttle.may_play(10_000));
    assert!(!throttle.may_play(10_000));
    assert!(!throttle.may_play(11_999));
    assert!(throttle.may_play(12_000));
    assert!(!throttle.may_play(13_000));
  }

  #[test]
  fn a_clock_set_back_does_not_silence_the_sounds() {
    let mut throttle = SoundThrottle::default();
    assert!(throttle.may_play(50_000));
    assert!(throttle.may_play(10_000), "the clock went back 40 s");
  }

  #[test]
  fn the_notification_keeps_the_system_sound_only_without_one_of_the_app() {
    let chime = MessageSound::BuiltIn(BuiltInSound::Chime);
    assert_eq!(notification_sound_for(chime, true), NotificationSound::Silent);
    assert_eq!(
      notification_sound_for(MessageSound::OwnFile, true),
      NotificationSound::Silent
    );
    assert_eq!(
      notification_sound_for(MessageSound::System, true),
      NotificationSound::System
    );
    assert_eq!(
      notification_sound_for(MessageSound::Silent, false),
      NotificationSound::Silent
    );
    // A tab that may not play sound yet: the system's sound rather than none.
    assert_eq!(notification_sound_for(chime, false), NotificationSound::System);
  }
}
