//! The incoming-call window of the desktop app (#650): a small window above all others while a call
//! rings and the app is in the background, like the in-app card (notifications.md, "Calls"). The
//! UI decides when it shows and what it says, in the user's language; the shell opens and closes
//! it, and reports its buttons as [`crate::os_notify::NotificationActionEvent`]s for the call's tag,
//! so answering from it takes the same path as answering from the notification or the card.

use serde::{Deserialize, Serialize};

use crate::os_notify::NotificationTag;

/// The label of the window in the shell; the UI renders only the call window under it.
pub const CALL_WINDOW_LABEL: &str = "incoming-call";

/// What the window shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallWindow {
  /// The call's notification tag ([`NotificationTag::call`]).
  pub tag: NotificationTag,
  /// "Incoming call".
  pub title: String,
  /// "Bob is calling you".
  pub text: String,
  /// The caller's avatar: initials on a hue, as the generated avatar.
  pub initials: String,
  pub avatar_hue: u16,
  pub decline_label: String,
  pub accept_label: String,
  pub theme: CallWindowTheme,
}

/// The app's theme, which the window follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallWindowTheme {
  Light,
  Dark,
  /// No theme of its own: the system's (also for a theme this shell doesn't know).
  #[default]
  #[serde(other)]
  System,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_window_round_trips_and_unknown_themes_follow_the_system() {
    let window = CallWindow {
      tag: NotificationTag::call(7),
      title: "Incoming call".to_owned(),
      text: "Bob is calling you".to_owned(),
      initials: "B".to_owned(),
      avatar_hue: 120,
      decline_label: "Decline".to_owned(),
      accept_label: "Accept".to_owned(),
      theme: CallWindowTheme::Dark,
    };
    let json = serde_json::to_string(&window).expect("encodes");
    assert!(json.contains(r#""tag":"call:7""#) && json.contains(r#""theme":"dark""#));
    assert_eq!(
      serde_json::from_str::<CallWindow>(&json).expect("decodes"),
      window
    );
    assert_eq!(
      serde_json::from_str::<CallWindowTheme>(r#""sepia""#).expect("decodes"),
      CallWindowTheme::System
    );
  }
}
