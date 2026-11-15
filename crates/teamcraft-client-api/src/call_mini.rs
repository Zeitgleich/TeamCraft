//! The small call window of the desktop app (#2077): while the user is in a call and the main
//! window is away (minimized, hidden to the tray or behind other windows), a small window above all
//! others shows who is talking, how long the call runs, and the controls that matter: mute,
//! camera, sound, hang up and "back to the call".
//!
//! The call engine and its state stay with the main window. The UI there builds a [`CallMini`]
//! from the state whenever it changes and hands it to the shell with the user's
//! [`CallMiniPolicy`]; the shell decides from the main window's focus whether the small window
//! shows, opens and closes it, and passes the window's buttons back to the main window as
//! [`crate::tray_menu::TrayAction`]s, the path the tray menu's call controls take. So nothing about
//! calls is done twice.

use serde::{Deserialize, Serialize};

use crate::call_window::CallWindowTheme;

/// The label of the window in the shell; the UI renders only the small call window under it.
pub const CALL_MINI_LABEL: &str = "call-mini";

/// The event the shell emits to the small window when what it shows changed; it reads it again
/// with `call_mini_info`.
pub const CALL_MINI_CHANGED_EVENT: &str = "call-mini-changed";

/// What the small window shows. Texts the window can't word itself (names) come worded; the rest
/// it words in the user's language.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallMini {
  /// "Call with Bob", or the meeting's title.
  pub title: String,
  /// Who talks right now (the loudest), with their avatar; `None` while nobody else talks.
  #[serde(default)]
  pub maybe_speaker: Option<CallMiniPerson>,
  /// How many people are in the call, this device's user included.
  pub people: u32,
  /// When this device entered the call (Unix milliseconds): the window's clock counts from here.
  #[serde(default)]
  pub maybe_joined_at_ms: Option<i64>,
  /// The connection was lost and the call rejoins.
  #[serde(default)]
  pub is_reconnecting: bool,
  pub is_muted: bool,
  pub is_camera_on: bool,
  /// The call's sound is off on this device (deafened).
  #[serde(default)]
  pub is_sound_off: bool,
  /// Leaving would leave people stuck in the lobby of the meeting the user organizes: hang up opens
  /// the call asking whom to hand it to first, as the call screen's button does (#1720).
  #[serde(default)]
  pub asks_handover: bool,
  pub theme: CallWindowTheme,
}

/// A person in the small window: name and the generated avatar's initials and hue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallMiniPerson {
  pub name: String,
  pub initials: String,
  pub avatar_hue: u16,
}

/// When the small call window shows (Settings > Audio and video).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallMiniPolicy {
  /// For the whole call, also while the main window is in front.
  Always,
  Never,
  /// Only while the main window is minimized, hidden or behind other windows (also for a value
  /// this shell doesn't know; serde wants that catch-all last).
  #[default]
  #[serde(other)]
  WhenAway,
}

impl CallMiniPolicy {
  pub const ALL: [Self; 3] = [Self::Always, Self::WhenAway, Self::Never];

  /// The settings' value and i18n key part.
  pub fn slug(self) -> &'static str {
    match self {
      Self::Always => "always",
      Self::WhenAway => "when-away",
      Self::Never => "never",
    }
  }

  pub fn from_slug(slug: &str) -> Self {
    Self::ALL
      .into_iter()
      .find(|policy| policy.slug() == slug)
      .unwrap_or_default()
  }

  /// Whether the window shows during a call, given whether the main window is away.
  pub fn shows(self, is_main_away: bool) -> bool {
    match self {
      Self::Always => true,
      Self::WhenAway => is_main_away,
      Self::Never => false,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_window_round_trips() {
    let mini = CallMini {
      title: "Call with Bob".to_owned(),
      maybe_speaker: Some(CallMiniPerson {
        name: "Bob".to_owned(),
        initials: "B".to_owned(),
        avatar_hue: 120,
      }),
      people: 2,
      maybe_joined_at_ms: Some(1_700_000_000_000),
      is_reconnecting: false,
      is_muted: true,
      is_camera_on: false,
      is_sound_off: false,
      asks_handover: false,
      theme: CallWindowTheme::Dark,
    };
    let json = serde_json::to_string(&mini).expect("encodes");
    assert!(json.contains(r#""is_muted":true"#) && json.contains(r#""theme":"dark""#));
    assert_eq!(serde_json::from_str::<CallMini>(&json).expect("decodes"), mini);
  }

  #[test]
  fn the_policy_decides_by_the_main_window() {
    assert!(CallMiniPolicy::Always.shows(false));
    assert!(CallMiniPolicy::Always.shows(true));
    assert!(CallMiniPolicy::WhenAway.shows(true));
    assert!(
      !CallMiniPolicy::WhenAway.shows(false),
      "the call is in front already"
    );
    assert!(!CallMiniPolicy::Never.shows(true));
  }

  #[test]
  fn policies_have_stable_names_and_unknown_ones_show_when_away() {
    assert_eq!(
      serde_json::to_string(&CallMiniPolicy::WhenAway).expect("encodes"),
      r#""when_away""#
    );
    assert_eq!(
      serde_json::from_str::<CallMiniPolicy>(r#""sometimes""#).expect("decodes"),
      CallMiniPolicy::WhenAway
    );
    for policy in CallMiniPolicy::ALL {
      assert_eq!(CallMiniPolicy::from_slug(policy.slug()), policy);
    }
    assert_eq!(CallMiniPolicy::from_slug("nonsense"), CallMiniPolicy::WhenAway);
  }
}
