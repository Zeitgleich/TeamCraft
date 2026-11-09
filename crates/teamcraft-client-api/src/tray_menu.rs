//! The tray icon's menu (macOS menu bar, Windows notification area, Linux StatusNotifier). The UI
//! knows the state (presence, call, unread) and the user's language, so it builds the menu as a
//! [`TrayMenuModel`]; the shell only draws it, and hands a click back as a [`TrayAction`] event for
//! the main window. Every action then takes the same path as the matching control in the window.
//! Shared by the shell, which owns the tray, and the UI, which owns the state.

use serde::{Deserialize, Serialize};

/// The event the shell emits to the main window, with a [`TrayAction`]'s id as its text, when a menu
/// item that needs the UI's state was chosen.
pub const TRAY_ACTION_EVENT: &str = "tray-action";

/// What a menu item does. The shell does [`TrayAction::is_shell_action`] ones itself; the rest go
/// to the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayAction {
  OpenApp,
  CheckForUpdates,
  Quit,
  StatusOnline,
  StatusBusy,
  StatusBeRightBack,
  StatusAway,
  StatusDoNotDisturb,
  DoNotDisturbForHour,
  DoNotDisturbUntilTomorrow,
  StatusOffline,
  AcceptCall,
  DeclineCall,
  Mute,
  Unmute,
  CameraOn,
  CameraOff,
  StopSharing,
  LeaveCall,
  OpenCall,
  /// Opens the dialog for the custom status in the main window.
  SetCustomStatus,
  /// Opens the new message dialog in the main window.
  NewMessage,
  /// Not a menu item: the global mute shortcut was pressed.
  ToggleMute,
  /// "Pause notifications → For 30 minutes" (do not disturb, as the focus button's shortest choice).
  PauseNotificationsBriefly,
  /// Ends a pause of the notifications (do not disturb), as the focus button does while it is on.
  ResumeNotifications,
  /// The host of a meeting: opens the call with "End for everyone" waiting to be confirmed.
  EndCallForEveryone,
}

impl TrayAction {
  pub const ALL: [Self; 26] = [
    Self::OpenApp,
    Self::CheckForUpdates,
    Self::Quit,
    Self::StatusOnline,
    Self::StatusBusy,
    Self::StatusBeRightBack,
    Self::StatusAway,
    Self::StatusDoNotDisturb,
    Self::DoNotDisturbForHour,
    Self::DoNotDisturbUntilTomorrow,
    Self::StatusOffline,
    Self::AcceptCall,
    Self::DeclineCall,
    Self::Mute,
    Self::Unmute,
    Self::CameraOn,
    Self::CameraOff,
    Self::StopSharing,
    Self::LeaveCall,
    Self::OpenCall,
    Self::SetCustomStatus,
    Self::NewMessage,
    Self::ToggleMute,
    Self::PauseNotificationsBriefly,
    Self::ResumeNotifications,
    Self::EndCallForEveryone,
  ];

  /// The menu item's id, and the text of the event.
  pub fn id(self) -> &'static str {
    match self {
      Self::OpenApp => "open_app",
      Self::CheckForUpdates => "check_for_updates",
      Self::Quit => "quit",
      Self::StatusOnline => "status_online",
      Self::StatusBusy => "status_busy",
      Self::StatusBeRightBack => "status_be_right_back",
      Self::StatusAway => "status_away",
      Self::StatusDoNotDisturb => "status_do_not_disturb",
      Self::DoNotDisturbForHour => "do_not_disturb_for_hour",
      Self::DoNotDisturbUntilTomorrow => "do_not_disturb_until_tomorrow",
      Self::StatusOffline => "status_offline",
      Self::AcceptCall => "accept_call",
      Self::DeclineCall => "decline_call",
      Self::Mute => "mute",
      Self::Unmute => "unmute",
      Self::CameraOn => "camera_on",
      Self::CameraOff => "camera_off",
      Self::StopSharing => "stop_sharing",
      Self::LeaveCall => "leave_call",
      Self::OpenCall => "open_call",
      Self::SetCustomStatus => "set_custom_status",
      Self::NewMessage => "new_message",
      Self::ToggleMute => "toggle_mute",
      Self::PauseNotificationsBriefly => "pause_notifications_briefly",
      Self::ResumeNotifications => "resume_notifications",
      Self::EndCallForEveryone => "end_call_for_everyone",
    }
  }

  pub fn from_id(id: &str) -> Option<Self> {
    Self::ALL.into_iter().find(|action| action.id() == id)
  }

  /// Whether the shell does it without the UI (the window may not even have a page). The shell
  /// still tells the UI about [`TrayAction::CheckForUpdates`], which then says what came of it.
  pub fn is_shell_action(self) -> bool {
    matches!(self, Self::OpenApp | Self::CheckForUpdates | Self::Quit)
  }

  /// Whether the main window is brought to the front before the UI handles it.
  pub fn shows_window(self) -> bool {
    matches!(
      self,
      Self::OpenApp
        | Self::OpenCall
        | Self::AcceptCall
        | Self::SetCustomStatus
        | Self::NewMessage
        | Self::EndCallForEveryone
    )
  }
}

/// What came of asking the shell for the global mute shortcut (`mute_shortcut_set`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MuteShortcutStatus {
  /// No shortcut asked for.
  #[default]
  Off,
  /// The shortcut works in every app (while a call runs).
  Ready,
  /// Not a shortcut the system understands.
  Invalid,
  /// Another app (or push-to-talk) already holds it.
  Taken,
  /// The desktop doesn't allow global shortcuts (e.g. Wayland without the portal), or no shell.
  Unavailable,
}

/// One line of the menu.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrayEntry {
  /// A line with a text. Without an action it is a header and cannot be chosen.
  Item {
    #[serde(default)]
    maybe_action: Option<TrayAction>,
    label: String,
    /// `Some` draws a check mark when `true` (the current status).
    #[serde(default)]
    maybe_checked: Option<bool>,
  },
  Separator,
  Submenu {
    label: String,
    entries: Vec<TrayEntry>,
  },
}

impl TrayEntry {
  pub fn action(action: TrayAction, label: impl Into<String>) -> Self {
    Self::Item {
      maybe_action: Some(action),
      label: label.into(),
      maybe_checked: None,
    }
  }

  pub fn choice(action: TrayAction, label: impl Into<String>, is_checked: bool) -> Self {
    Self::Item {
      maybe_action: Some(action),
      label: label.into(),
      maybe_checked: Some(is_checked),
    }
  }

  pub fn header(label: impl Into<String>) -> Self {
    Self::Item {
      maybe_action: None,
      label: label.into(),
      maybe_checked: None,
    }
  }
}

/// Which look of the tray icon fits the state: a call shows a dot, a muted microphone another.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrayCallState {
  #[default]
  Idle,
  InCall,
  /// In a call with the microphone off.
  Muted,
}

/// The whole menu, as the UI wants it now. Empty entries: the UI has none (sign-in, closed), and
/// the shell shows its plain menu.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrayMenuModel {
  pub entries: Vec<TrayEntry>,
  #[serde(default)]
  pub call_state: TrayCallState,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn ids_round_trip_and_match_the_serde_names() {
    for action in TrayAction::ALL {
      assert_eq!(TrayAction::from_id(action.id()), Some(action));
      assert_eq!(
        serde_json::to_string(&action).expect("encodes"),
        format!("\"{}\"", action.id()),
        "the serde name is the id"
      );
    }
    assert_eq!(TrayAction::from_id("nonsense"), None);
  }

  #[test]
  fn the_shell_keeps_only_what_needs_no_page() {
    let shell: Vec<_> = TrayAction::ALL
      .into_iter()
      .filter(|action| action.is_shell_action())
      .collect();
    assert_eq!(
      shell,
      [TrayAction::OpenApp, TrayAction::CheckForUpdates, TrayAction::Quit]
    );
    assert!(TrayAction::OpenCall.shows_window() && !TrayAction::Mute.shows_window());
    assert!(TrayAction::SetCustomStatus.shows_window() && TrayAction::NewMessage.shows_window());
    assert!(
      TrayAction::EndCallForEveryone.shows_window(),
      "ending for everyone is confirmed in the window"
    );
    assert!(
      !TrayAction::ToggleMute.shows_window(),
      "the shortcut works in the background"
    );
  }

  #[test]
  fn a_model_survives_the_wire() {
    let model = TrayMenuModel {
      entries: vec![
        TrayEntry::header("Call with Bob"),
        TrayEntry::choice(TrayAction::StatusAway, "Away", true),
        TrayEntry::Separator,
        TrayEntry::Submenu {
          label: "Status".to_owned(),
          entries: vec![TrayEntry::action(TrayAction::Quit, "Quit")],
        },
      ],
      call_state: TrayCallState::Muted,
    };
    let json = serde_json::to_string(&model).expect("encodes");
    assert_eq!(
      serde_json::from_str::<TrayMenuModel>(&json).expect("decodes"),
      model
    );
  }
}
