//! System notifications as the UI hands them to the desktop shell, and the actions the user takes
//! on them (notifications.md, "Desktop notifications"). The UI decides what to show, in the user's
//! language and within the preview settings; the shell only displays, replaces, withdraws and
//! reports clicks. Shared by the shell and the UI, so it can serve the tray menu too.

use serde::{Deserialize, Serialize};

/// What the shell emits to the webview when the user clicked a notification or one of its actions
/// (payload: [`NotificationActionEvent`] as JSON text).
pub const NOTIFICATION_ACTION_EVENT: &str = "notification-action";

/// Tag prefix of an incoming call's notification; the conversation ID follows.
const CALL_TAG_PREFIX: &str = "call:";
/// Tag prefix of a missed call's notification; the conversation ID follows.
const MISSED_CALL_TAG_PREFIX: &str = "missed-call:";
/// Tag prefix of a meeting reminder; the meeting ID follows.
const MEETING_TAG_PREFIX: &str = "meeting:";
/// Tag prefix of a note to self (`/remind`); its due time follows.
const REMINDER_TAG_PREFIX: &str = "reminder:";
/// Tag prefix of an admin's request to join a private channel; `{conversation}:{user}` follows.
const JOIN_REQUEST_TAG_PREFIX: &str = "admin-join:";

/// One notification to show. A newer one with the same tag replaces it; withdrawing the tag
/// removes it, where the platform can.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsNotification {
  pub tag: NotificationTag,
  pub kind: OsNotificationKind,
  pub title: String,
  #[serde(rename = "body", default, skip_serializing_if = "Option::is_none")]
  pub maybe_body: Option<String>,
  /// Buttons, in order; platforms show as many as they can (Linux servers usually three).
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub actions: Vec<NotificationButton>,
  /// A text field to answer in, where the platform has one (Windows, Linux servers with
  /// `inline-reply`).
  #[serde(rename = "reply", default, skip_serializing_if = "Option::is_none")]
  pub maybe_reply: Option<ReplyField>,
  /// Whether the system plays its notification sound with it (`message_sound`).
  #[serde(default, skip_serializing_if = "NotificationSound::is_system")]
  pub sound: NotificationSound,
}

/// Whether the system plays its own sound with a notification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSound {
  /// The system's notification sound, where the user's system settings allow one.
  #[default]
  System,
  /// None: the app plays its own message sound, or the user chose none.
  Silent,
  /// A value of a newer version: the system's sound.
  #[serde(other)]
  Unknown,
}

/// The texts of a notification's reply field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyField {
  pub placeholder: String,
  /// The button that sends (Windows).
  pub send_label: String,
}

/// The tag of a notification: the conversation ID for messages (as the service worker's push
/// notifications), prefixed for calls and meetings.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NotificationTag(String);

/// What a notification is about; decides sound, urgency and how long it stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OsNotificationKind {
  Message,
  /// Rings: stays until answered, declined or withdrawn; the ringtone plays separately.
  IncomingCall,
  MissedCall,
  MeetingReminder,
  /// `/remind` and other notes to self.
  Reminder,
  /// An admin asks to join a private channel the user owns (admin.md §2.4).
  JoinRequest,
  #[serde(other)]
  Unknown,
}

/// One button.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationButton {
  pub action: NotificationAction,
  pub label: String,
}

/// What the user did with a notification.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationAction {
  /// Clicked the notification itself.
  Open,
  AcceptAudio,
  AcceptVideo,
  Decline,
  /// Call back after a missed call.
  CallBack,
  JoinMeeting,
  MarkRead,
  /// Sent text from the notification's reply field.
  Reply,
  /// Let in the admin who asked to join (an owner's decision; the answer to it is `Decline`).
  Approve,
  #[serde(other)]
  Unknown,
}

/// What the shell reports to the webview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationActionEvent {
  pub tag: NotificationTag,
  pub action: NotificationAction,
  /// The typed reply of [`NotificationAction::Reply`].
  #[serde(rename = "text", default, skip_serializing_if = "Option::is_none")]
  pub maybe_text: Option<String>,
}

/// What a tag points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagTarget {
  Conversation(String),
  Call(String),
  MissedCall(String),
  Meeting(String),
  Reminder,
  /// An admin's request to join: the conversation and the admin, as text.
  JoinRequest {
    conversation: String,
    user: String,
  },
}

impl NotificationTag {
  /// The tag of a conversation's message notifications.
  pub fn conversation(id: impl ToString) -> Self {
    Self(id.to_string())
  }

  pub fn call(conversation: impl ToString) -> Self {
    Self(format!("{CALL_TAG_PREFIX}{}", conversation.to_string()))
  }

  pub fn missed_call(conversation: impl ToString) -> Self {
    Self(format!("{MISSED_CALL_TAG_PREFIX}{}", conversation.to_string()))
  }

  pub fn meeting(meeting: impl ToString) -> Self {
    Self(format!("{MEETING_TAG_PREFIX}{}", meeting.to_string()))
  }

  pub fn reminder(due_at: impl ToString) -> Self {
    Self(format!("{REMINDER_TAG_PREFIX}{}", due_at.to_string()))
  }

  pub fn join_request(conversation: impl ToString, user: impl ToString) -> Self {
    Self(format!(
      "{JOIN_REQUEST_TAG_PREFIX}{}:{}",
      conversation.to_string(),
      user.to_string()
    ))
  }

  /// A tag as a platform handed it back.
  pub fn from_raw(raw: impl Into<String>) -> Self {
    Self(raw.into())
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }

  /// What the tag points at; the ID stays text for the caller to parse.
  pub fn target(&self) -> TagTarget {
    let tag = self.0.as_str();
    if let Some(id) = tag.strip_prefix(CALL_TAG_PREFIX) {
      TagTarget::Call(id.to_owned())
    } else if let Some(id) = tag.strip_prefix(MISSED_CALL_TAG_PREFIX) {
      TagTarget::MissedCall(id.to_owned())
    } else if let Some(id) = tag.strip_prefix(MEETING_TAG_PREFIX) {
      TagTarget::Meeting(id.to_owned())
    } else if tag.starts_with(REMINDER_TAG_PREFIX) {
      TagTarget::Reminder
    } else if let Some((conversation, user)) = tag
      .strip_prefix(JOIN_REQUEST_TAG_PREFIX)
      .and_then(|rest| rest.split_once(':'))
    {
      TagTarget::JoinRequest {
        conversation: conversation.to_owned(),
        user: user.to_owned(),
      }
    } else {
      TagTarget::Conversation(tag.to_owned())
    }
  }
}

impl NotificationAction {
  /// The identifier platforms carry for the action (freedesktop action keys, toast arguments).
  pub fn key(self) -> &'static str {
    match self {
      Self::Open => "default",
      Self::AcceptAudio => "accept-audio",
      Self::AcceptVideo => "accept-video",
      Self::Decline => "decline",
      Self::CallBack => "call-back",
      Self::JoinMeeting => "join-meeting",
      Self::MarkRead => "mark-read",
      Self::Reply => "inline-reply",
      Self::Approve => "approve",
      Self::Unknown => "unknown",
    }
  }

  /// Parses [`Self::key`]; anything else is [`Self::Unknown`].
  pub fn from_key(key: &str) -> Self {
    [
      Self::Open,
      Self::AcceptAudio,
      Self::AcceptVideo,
      Self::Decline,
      Self::CallBack,
      Self::JoinMeeting,
      Self::MarkRead,
      Self::Reply,
      Self::Approve,
    ]
    .into_iter()
    .find(|action| action.key() == key)
    .unwrap_or(Self::Unknown)
  }

  /// Whether the action needs the window in front (answering, joining, opening).
  pub fn brings_window_up(self) -> bool {
    matches!(
      self,
      Self::Open | Self::AcceptAudio | Self::AcceptVideo | Self::CallBack | Self::JoinMeeting
    )
  }
}

impl OsNotificationKind {
  /// Calls ring until something happens; everything else may time out like any notification.
  pub fn is_ringing(self) -> bool {
    self == Self::IncomingCall
  }
}

impl OsNotification {
  /// The desktop shows it without the system's sound: a ringing call (the app plays the ringtone),
  /// or a message whose sound the app plays itself, or none.
  pub fn is_quiet(&self) -> bool {
    self.kind.is_ringing() || self.sound == NotificationSound::Silent
  }
}

impl NotificationSound {
  fn is_system(&self) -> bool {
    matches!(self, Self::System | Self::Unknown)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tags_name_what_they_point_at() {
    assert_eq!(
      NotificationTag::conversation(42).target(),
      TagTarget::Conversation("42".into())
    );
    assert_eq!(NotificationTag::call(42).target(), TagTarget::Call("42".into()));
    assert_eq!(
      NotificationTag::missed_call(42).target(),
      TagTarget::MissedCall("42".into())
    );
    assert_eq!(
      NotificationTag::meeting(7).target(),
      TagTarget::Meeting("7".into())
    );
    assert_eq!(NotificationTag::reminder(99).target(), TagTarget::Reminder);
    assert_eq!(
      NotificationTag::join_request(42, 7).target(),
      TagTarget::JoinRequest {
        conversation: "42".into(),
        user: "7".into()
      }
    );
    assert_eq!(NotificationTag::conversation(42).as_str(), "42");
  }

  #[test]
  fn a_notification_keeps_the_system_sound_unless_told_otherwise() {
    let older: OsNotification =
      serde_json::from_str(r#"{"tag":"42","kind":"message","title":"Alice"}"#).unwrap();
    assert_eq!(older.sound, NotificationSound::System);
    assert!(!older.is_quiet());
    assert!(!serde_json::to_string(&older).unwrap().contains("sound"));
    let quiet = OsNotification {
      sound: NotificationSound::Silent,
      ..older.clone()
    };
    assert!(quiet.is_quiet());
    assert!(
      serde_json::to_string(&quiet)
        .unwrap()
        .contains(r#""sound":"silent""#)
    );
    let ringing = OsNotification {
      kind: OsNotificationKind::IncomingCall,
      ..older
    };
    assert!(ringing.is_quiet());
  }

  #[test]
  fn action_keys_round_trip_and_unknown_keys_are_tolerated() {
    for action in [
      NotificationAction::Open,
      NotificationAction::AcceptAudio,
      NotificationAction::AcceptVideo,
      NotificationAction::Decline,
      NotificationAction::CallBack,
      NotificationAction::JoinMeeting,
      NotificationAction::MarkRead,
      NotificationAction::Reply,
      NotificationAction::Approve,
    ] {
      assert_eq!(NotificationAction::from_key(action.key()), action);
    }
    assert_eq!(
      NotificationAction::from_key("snooze"),
      NotificationAction::Unknown
    );
  }

  #[test]
  fn the_event_reads_as_the_webview_expects() {
    let event = NotificationActionEvent {
      tag: NotificationTag::conversation(5),
      action: NotificationAction::Reply,
      maybe_text: Some("on my way".into()),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(
      json,
      serde_json::json!({ "tag": "5", "action": "reply", "text": "on my way" })
    );
    let unknown: NotificationActionEvent =
      serde_json::from_value(serde_json::json!({ "tag": "5", "action": "snooze" })).unwrap();
    assert_eq!(unknown.action, NotificationAction::Unknown);
  }
}
