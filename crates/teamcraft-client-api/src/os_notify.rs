//! System notifications as the UI hands them to the desktop shell, and the actions the user takes
//! on them (notifications.md, "Desktop notifications"). The UI decides what to show, in the user's
//! language and within the preview settings; the shell only displays, replaces, withdraws and
//! reports clicks. Shared by the shell and the UI, so it can serve the tray menu too.

use serde::{Deserialize, Serialize};
use teamcraft_proto::Base64Bytes;

use crate::media::MediaKey;

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
/// Side of a notification picture in pixels: the generated tile the UI draws and what the shell
/// scales a cached picture to (the services show it at 48 to 80 points, so this is sharp at 2x).
pub const NOTIFICATION_PICTURE_PX: u32 = 128;

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
  /// Who wrote or where (#543): the sender's avatar for direct messages, the channel's or space's
  /// picture for channels. The UI only sets it where the notification may name names.
  #[serde(rename = "picture", default, skip_serializing_if = "Option::is_none")]
  pub maybe_picture: Option<NotificationPicture>,
}

/// The picture beside a notification's text. The shell reads [`Self::maybe_key`] from the
/// persistent media cache (#749), where the app keeps every picture it showed, and never fetches
/// one for a notification; without a cached copy it shows the generated fallback. Pictures are
/// cut to their shape and scaled to [`NOTIFICATION_PICTURE_PX`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationPicture {
  pub shape: PictureShape,
  /// The cached picture: an avatar, a channel's or a space's picture.
  #[serde(rename = "key", default, skip_serializing_if = "Option::is_none")]
  pub maybe_key: Option<MediaKey>,
  /// What shows while the cache has no picture: the generated initials tile as the app draws it,
  /// as PNG of [`NOTIFICATION_PICTURE_PX`] pixels, already in its shape.
  #[serde(rename = "fallback_png", default, skip_serializing_if = "Option::is_none")]
  pub maybe_fallback_png: Option<Base64Bytes>,
}

/// The outline a picture is cut to, as the app shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PictureShape {
  /// A person's avatar.
  Circle,
  /// A space or channel tile.
  RoundedSquare,
  /// From a newer UI: shown as a plain square.
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

impl PictureShape {
  /// Radius of the outline's corners as a share of the side: a circle, the app's space tile
  /// (12 px on 40 px, `--radius-md`), or none.
  pub fn corner_share(self) -> f32 {
    match self {
      Self::Circle => 0.5,
      Self::RoundedSquare => 0.3,
      Self::Unknown => 0.0,
    }
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
  fn a_picture_travels_with_its_key_and_fallback_and_unknown_shapes_are_tolerated() {
    let user: teamcraft_proto::UserId = serde_json::from_value(serde_json::json!("7")).unwrap();
    let picture = NotificationPicture {
      shape: PictureShape::Circle,
      maybe_key: Some(MediaKey::Avatar(user)),
      maybe_fallback_png: Some(Base64Bytes::new(b"png".to_vec())),
    };
    let json = serde_json::to_value(&picture).unwrap();
    assert_eq!(
      json,
      serde_json::json!({
        "shape": "circle",
        "key": { "kind": "avatar", "id": "7" },
        "fallback_png": "cG5n"
      })
    );
    assert_eq!(
      serde_json::from_value::<NotificationPicture>(json).unwrap(),
      picture
    );
    let newer: NotificationPicture =
      serde_json::from_value(serde_json::json!({ "shape": "hexagon" })).unwrap();
    assert_eq!(newer.shape, PictureShape::Unknown);
    assert_eq!(newer.maybe_key, None);
    assert_eq!(newer.shape.corner_share(), 0.0, "a plain square");
  }

  #[test]
  fn a_notification_without_a_picture_reads_as_before() {
    let json = serde_json::json!({ "tag": "5", "kind": "message", "title": "Bob" });
    let notification: OsNotification = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(notification.maybe_picture, None);
    assert_eq!(serde_json::to_value(&notification).unwrap(), json);
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
