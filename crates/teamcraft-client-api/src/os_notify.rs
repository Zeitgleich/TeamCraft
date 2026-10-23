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
  /// A text field to answer in, where the platform has one (Windows, macOS, Linux servers with
  /// `inline-reply`). Its placeholder text.
  #[serde(
    rename = "reply_placeholder",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_reply_placeholder: Option<String>,
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
    assert_eq!(NotificationTag::conversation(42).as_str(), "42");
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
