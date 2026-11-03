//! The sound for new messages on desktop and web (notifications.md, "Message sounds", #800): the
//! system's notification sound, none, one of the app's own sounds, or a file the user picked on
//! this device. Shared by the UI, which decides when a sound plays, and the desktop shell, which
//! plays it and keeps the picked file.
//!
//! The app's own sounds are synthesized (`teamcraft_media::message_sounds`), like the ringtone:
//! there are no sound files and nothing to license. A picked file stays on the device it was
//! picked on: it is never uploaded or synced, so the server never sees it.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::os_notify::NotificationSound;

/// Stored word of [`MessageSound::System`].
const SYSTEM_SLUG: &str = "system";
/// Stored word of [`MessageSound::Silent`].
const SILENT_SLUG: &str = "none";
/// Stored word of [`MessageSound::OwnFile`].
const OWN_FILE_SLUG: &str = "own_file";
const MIB: u64 = 1024 * 1024;
/// Upper bounds of the own file's limits, whatever the stored settings say: they bound the memory
/// a decoded sound takes (robustness.md, "Large data").
const MAX_FILE_BYTES_CAP: u64 = 16 * MIB;
const MAX_PLAY_SECONDS_CAP: u32 = 30;

/// What plays for a new message. Stored as one word (`system`, `none`, a built-in sound's slug,
/// `own_file`); a word this version doesn't know (a sound of a newer version) reads as the default
/// built-in sound, so a newer choice never silences an older app.
///
/// One value for every kind of message for now; direct messages, mentions and channel messages
/// can each get one of their own later (a field per kind, each of this type).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum MessageSound {
  /// The notification's own sound, as the system plays it; what TeamCraft always did.
  #[default]
  System,
  /// No sound at all.
  Silent,
  /// One of the app's own sounds.
  BuiltIn(BuiltInSound),
  /// The file the user picked on this device ([`OwnSoundStatus`]).
  OwnFile,
}

/// One of the app's own, synthesized sounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltInSound {
  /// Two soft bell notes, rising.
  Chime,
  /// One bright, short bell.
  Ping,
  /// A round little bubble.
  Pop,
  /// Two wooden mallet notes.
  Marimba,
  /// Two knocks on a door.
  Knock,
  /// The ringtone's electric piano, two notes.
  Keys,
  /// A playful two-syllable "uh-oh" in the spirit of the messengers of the early 2000s: a voice
  /// of our own, synthesized.
  UhOh,
}

/// The limits of the own file: settings with defaults (robustness.md), read when a file is picked
/// and when it is decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OwnSoundLimits {
  /// Larger files are refused when picked.
  pub max_file_bytes: u64,
  /// Longer sounds are cut off (faded out) after this many seconds.
  pub max_play_seconds: u32,
}

/// The own file as this device keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnSoundInfo {
  /// The picked file's name, shown in the settings.
  pub file_name: String,
  /// How long it plays, cut to the limit.
  pub play_ms: u32,
}

/// Whether this device has an own file, as the settings show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum OwnSoundStatus {
  /// None was picked on this device.
  NotPicked,
  Ready(OwnSoundInfo),
  /// One was picked but is gone or can't be read any more: the default sound plays instead.
  Unavailable,
}

/// The answer to "Own file…".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum OwnSoundPick {
  /// The user closed the dialog without a file.
  Cancelled,
  /// Checked and kept; it plays from now on.
  Stored(OwnSoundInfo),
  /// Not kept; the file kept before (if any) stays.
  Refused { reason: OwnSoundRefusal },
}

/// Why a picked file was not kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnSoundRefusal {
  /// Larger than [`OwnSoundLimits::max_file_bytes`].
  TooLarge,
  /// It doesn't decode as sound on this device (or has none in it).
  NotAudio,
  /// It could not be read.
  Unreadable,
  #[serde(other)]
  Unknown,
}

/// What playing a message sound did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoundPlayed {
  Played,
  /// The own file is gone or can't be read: the default sound played instead.
  FellBack,
  /// Nothing played: the desktop's own do-not-disturb is on, or no output device.
  Quiet,
  #[serde(other)]
  Unknown,
}

impl MessageSound {
  /// The choices in the order the settings list them.
  pub fn choices() -> impl Iterator<Item = MessageSound> {
    [Self::System, Self::Silent]
      .into_iter()
      .chain(BuiltInSound::ALL.into_iter().map(Self::BuiltIn))
      .chain([Self::OwnFile])
  }

  pub fn slug(self) -> &'static str {
    match self {
      Self::System => SYSTEM_SLUG,
      Self::Silent => SILENT_SLUG,
      Self::BuiltIn(sound) => sound.slug(),
      Self::OwnFile => OWN_FILE_SLUG,
    }
  }

  /// The choice a stored word names; an unknown word is the default built-in sound.
  pub fn from_slug(slug: &str) -> Self {
    match slug {
      SYSTEM_SLUG => Self::System,
      SILENT_SLUG => Self::Silent,
      OWN_FILE_SLUG => Self::OwnFile,
      other => Self::BuiltIn(BuiltInSound::from_slug(other).unwrap_or(BuiltInSound::DEFAULT)),
    }
  }

  /// The app plays this sound itself (on the desktop's speakers or in the page).
  pub fn is_played_by_app(self) -> bool {
    matches!(self, Self::BuiltIn(_) | Self::OwnFile)
  }

  /// Whether the system plays its own sound with the notification: only for [`Self::System`]; the
  /// app's own sound replaces it, and "none" means none.
  pub fn notification_sound(self) -> NotificationSound {
    match self {
      Self::System => NotificationSound::System,
      Self::Silent | Self::BuiltIn(_) | Self::OwnFile => NotificationSound::Silent,
    }
  }
}

impl From<String> for MessageSound {
  fn from(slug: String) -> Self {
    Self::from_slug(&slug)
  }
}

impl From<MessageSound> for String {
  fn from(sound: MessageSound) -> Self {
    sound.slug().to_owned()
  }
}

impl BuiltInSound {
  pub const ALL: [BuiltInSound; 7] = [
    Self::Chime,
    Self::Ping,
    Self::Pop,
    Self::Marimba,
    Self::Knock,
    Self::Keys,
    Self::UhOh,
  ];
  /// What a missing or unreadable own file falls back to.
  pub const DEFAULT: BuiltInSound = Self::Chime;

  pub fn slug(self) -> &'static str {
    match self {
      Self::Chime => "chime",
      Self::Ping => "ping",
      Self::Pop => "pop",
      Self::Marimba => "marimba",
      Self::Knock => "knock",
      Self::Keys => "keys",
      Self::UhOh => "uh_oh",
    }
  }

  pub fn from_slug(slug: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|sound| sound.slug() == slug)
  }
}

impl Default for OwnSoundLimits {
  fn default() -> Self {
    Self {
      max_file_bytes: MIB,
      max_play_seconds: 5,
    }
  }
}

impl OwnSoundLimits {
  /// The choices of "Play at most" in the settings.
  pub const PLAY_SECONDS_CHOICES: [u32; 3] = [2, 5, 10];

  /// The largest file accepted, at most 16 MiB.
  pub fn max_bytes(self) -> u64 {
    self.max_file_bytes.clamp(1, MAX_FILE_BYTES_CAP)
  }

  /// How long a file plays at most: at least one second, at most half a minute.
  pub fn max_play(self) -> Duration {
    Duration::from_secs(u64::from(self.max_play_seconds.clamp(1, MAX_PLAY_SECONDS_CAP)))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn every_choice_is_stored_as_its_word_and_read_back() {
    for sound in MessageSound::choices() {
      let json = serde_json::to_string(&sound).unwrap();
      assert_eq!(json, format!("\"{}\"", sound.slug()));
      assert_eq!(serde_json::from_str::<MessageSound>(&json).unwrap(), sound);
    }
    assert_eq!(MessageSound::choices().count(), BuiltInSound::ALL.len() + 3);
    assert_eq!(
      serde_json::to_string(&MessageSound::BuiltIn(BuiltInSound::UhOh)).unwrap(),
      "\"uh_oh\""
    );
  }

  #[test]
  fn a_word_of_a_newer_version_plays_the_default_sound() {
    assert_eq!(
      serde_json::from_str::<MessageSound>("\"trumpet\"").unwrap(),
      MessageSound::BuiltIn(BuiltInSound::DEFAULT)
    );
  }

  #[test]
  fn the_system_sound_is_the_default_and_only_it_keeps_the_notification_sound() {
    assert_eq!(MessageSound::default(), MessageSound::System);
    for sound in MessageSound::choices() {
      let expected = if sound == MessageSound::System {
        NotificationSound::System
      } else {
        NotificationSound::Silent
      };
      assert_eq!(sound.notification_sound(), expected, "{sound:?}");
      assert_eq!(
        sound.is_played_by_app(),
        matches!(sound, MessageSound::BuiltIn(_) | MessageSound::OwnFile)
      );
    }
  }

  #[test]
  fn limits_have_defaults_and_are_bounded() {
    let limits = OwnSoundLimits::default();
    assert_eq!(limits.max_bytes(), 1024 * 1024);
    assert_eq!(limits.max_play(), Duration::from_secs(5));
    let wild = OwnSoundLimits {
      max_file_bytes: u64::MAX,
      max_play_seconds: 0,
    };
    assert_eq!(wild.max_bytes(), MAX_FILE_BYTES_CAP);
    assert_eq!(wild.max_play(), Duration::from_secs(1));
    let partial: OwnSoundLimits = serde_json::from_str(r#"{"max_play_seconds":10}"#).unwrap();
    assert_eq!(partial.max_file_bytes, 1024 * 1024);
    assert_eq!(partial.max_play(), Duration::from_secs(10));
  }

  #[test]
  fn the_shell_answers_are_tagged_and_unknown_values_are_tolerated() {
    let stored = OwnSoundPick::Stored(OwnSoundInfo {
      file_name: "bell.ogg".to_owned(),
      play_ms: 1200,
    });
    assert_eq!(
      serde_json::to_string(&stored).unwrap(),
      r#"{"outcome":"stored","file_name":"bell.ogg","play_ms":1200}"#
    );
    let refused: OwnSoundPick =
      serde_json::from_str(r#"{"outcome":"refused","reason":"brand_new"}"#).unwrap();
    assert_eq!(
      refused,
      OwnSoundPick::Refused {
        reason: OwnSoundRefusal::Unknown
      }
    );
    assert_eq!(
      serde_json::from_str::<OwnSoundStatus>(r#"{"state":"unavailable"}"#).unwrap(),
      OwnSoundStatus::Unavailable
    );
    assert_eq!(
      serde_json::from_str::<SoundPlayed>("\"later\"").unwrap(),
      SoundPlayed::Unknown
    );
  }
}
