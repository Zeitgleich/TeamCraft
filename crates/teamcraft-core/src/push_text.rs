//! The words of a push notification shown by code that has no translations of its own: the
//! browser's service worker (e2ee.md §7). The page leaves the translated texts it needs in the
//! user's language ([`NotificationLabels`]); names and message text come from the [`PushPreview`].
//!
//! The rules are the native apps' (iOS `NotificationText`, `NotificationContentBuilder`), so a
//! notification reads the same everywhere (issue #644): a channel or group DM is titled by the
//! conversation and names the sender, a DM is titled by the sender; the text is the message, a
//! "Photo"/"Video"/"Audio" label, the file name, "New message from {name}" or "New message"; a call
//! says who calls. Web notifications have no subtitle, so the sender the apps show there leads the
//! body ("Alex: text"). A notification never goes out empty.

use serde::{Deserialize, Serialize};
use teamcraft_proto::PushKind;

use crate::push_preview::{AttachmentKind, PreviewContent, PushPreview};

/// The product name: the title when nothing names the conversation or the sender. Not translated.
pub const FALLBACK_TITLE: &str = "TeamCraft";
/// Where a label with a name has the name (the page translates it with this as the argument).
pub const NAME_PLACEHOLDER: &str = "{name}";

/// Translated notification texts, as the page leaves them for the service worker. Each may be
/// missing (a page of an older version): English is used then.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationLabels {
  /// "New message".
  #[serde(rename = "new_message", default, skip_serializing_if = "Option::is_none")]
  pub maybe_new_message: Option<String>,
  /// "New message from {name}", with [`NAME_PLACEHOLDER`] for the sender.
  #[serde(rename = "new_message_from", default, skip_serializing_if = "Option::is_none")]
  pub maybe_new_message_from: Option<String>,
  /// "Photo", for a message whose text is a picture.
  #[serde(rename = "photo", default, skip_serializing_if = "Option::is_none")]
  pub maybe_photo: Option<String>,
  #[serde(rename = "video", default, skip_serializing_if = "Option::is_none")]
  pub maybe_video: Option<String>,
  #[serde(rename = "audio", default, skip_serializing_if = "Option::is_none")]
  pub maybe_audio: Option<String>,
  /// "Incoming call", the title of a ring.
  #[serde(rename = "call_title", default, skip_serializing_if = "Option::is_none")]
  pub maybe_call_title: Option<String>,
  /// "Incoming call from {name}".
  #[serde(rename = "call_from", default, skip_serializing_if = "Option::is_none")]
  pub maybe_call_from: Option<String>,
  /// "Someone is calling you", a ring whose caller is not known.
  #[serde(rename = "call_unknown", default, skip_serializing_if = "Option::is_none")]
  pub maybe_call_unknown: Option<String>,
  /// The body of the test push from the notification diagnostics.
  #[serde(rename = "test_body", default, skip_serializing_if = "Option::is_none")]
  pub maybe_test_body: Option<String>,
  /// "Mark as read", the notification's button; without it there is no button.
  #[serde(rename = "mark_read", default, skip_serializing_if = "Option::is_none")]
  pub maybe_mark_read: Option<String>,
}

/// Title and body of one notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationText {
  pub title: String,
  pub body: String,
}

impl NotificationLabels {
  /// Reads the labels the page left; anything unreadable counts as none (English then).
  pub fn from_json(json: &str) -> Self {
    serde_json::from_str(json).unwrap_or_default()
  }

  /// The notification for a push that could not be opened at all, like the apps' generic one.
  pub fn generic(&self) -> NotificationText {
    NotificationText {
      title: FALLBACK_TITLE.to_owned(),
      body: self.new_message(),
    }
  }

  fn new_message(&self) -> String {
    label(&self.maybe_new_message, "New message")
  }

  fn new_message_from(&self, name: &str) -> String {
    with_name(&self.maybe_new_message_from, "New message from {name}", name)
  }

  fn kind(&self, kind: AttachmentKind) -> Option<String> {
    match kind {
      AttachmentKind::Image => Some(label(&self.maybe_photo, "Photo")),
      AttachmentKind::Video => Some(label(&self.maybe_video, "Video")),
      AttachmentKind::Audio => Some(label(&self.maybe_audio, "Audio")),
      AttachmentKind::Other => None,
    }
  }
}

/// Title and body for a push preview, by the native apps' rules (see the module docs).
pub fn notification_text(preview: &PushPreview, labels: &NotificationLabels) -> NotificationText {
  let maybe_sender = non_blank(preview.maybe_sender_name.as_deref());
  let maybe_conversation = non_blank(preview.maybe_conversation_name.as_deref());
  match preview.wake_up.kind {
    PushKind::Call => {
      let maybe_caller = maybe_sender.or(maybe_conversation);
      NotificationText {
        title: label(&labels.maybe_call_title, "Incoming call"),
        body: match maybe_caller {
          Some(caller) => with_name(&labels.maybe_call_from, "Incoming call from {name}", caller),
          None => label(&labels.maybe_call_unknown, "Someone is calling you"),
        },
      }
    },
    PushKind::Test => NotificationText {
      title: FALLBACK_TITLE.to_owned(),
      body: label(
        &labels.maybe_test_body,
        "Test notification: push notifications reach this device.",
      ),
    },
    _ => message_text(preview, labels, maybe_sender, maybe_conversation),
  }
}

fn message_text(
  preview: &PushPreview,
  labels: &NotificationLabels,
  maybe_sender: Option<&str>,
  maybe_conversation: Option<&str>,
) -> NotificationText {
  let sender_name = maybe_sender.or(maybe_conversation).unwrap_or(FALLBACK_TITLE);
  let text = match &preview.content {
    PreviewContent::Text(text) if non_blank(Some(text)).is_some() => text.clone(),
    PreviewContent::File { file_name, kind } => labels
      .kind(*kind)
      .or_else(|| non_blank(Some(file_name)).map(str::to_owned))
      .unwrap_or_else(|| labels.new_message()),
    _ => match maybe_sender {
      Some(sender) => labels.new_message_from(sender),
      None => labels.new_message(),
    },
  };
  match maybe_conversation.filter(|_| !preview.is_direct) {
    // The apps' subtitle (the sender) leads the body: web notifications have no subtitle.
    Some(conversation) => NotificationText {
      title: conversation.to_owned(),
      body: match maybe_sender {
        Some(sender) => format!("{sender}: {text}"),
        None => text,
      },
    },
    None => NotificationText {
      title: sender_name.to_owned(),
      body: text,
    },
  }
}

fn label(maybe: &Option<String>, english: &str) -> String {
  non_blank(maybe.as_deref()).unwrap_or(english).to_owned()
}

/// A label with the name put in; one that lost its placeholder falls back to English.
fn with_name(maybe: &Option<String>, english: &str, name: &str) -> String {
  non_blank(maybe.as_deref())
    .filter(|template| template.contains(NAME_PLACEHOLDER))
    .unwrap_or(english)
    .replace(NAME_PLACEHOLDER, name)
}

fn non_blank(maybe: Option<&str>) -> Option<&str> {
  maybe.filter(|text| !text.trim().is_empty())
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ConversationId, Id, PushWakeUp, Snowflake};

  use super::*;

  mod messages {
    use super::*;

    #[test]
    fn a_direct_message_is_titled_by_its_sender() {
      let text = notification_text(
        &preview(true, Some("Alex"), Some("Alex"), text("lunch?")),
        &german(),
      );
      assert_eq!(text, expected("Alex", "lunch?"));
    }

    #[test]
    fn a_channel_message_is_titled_by_the_channel_and_names_the_sender() {
      let text = notification_text(
        &preview(false, Some("Alex"), Some("Design"), text("lunch?")),
        &german(),
      );
      assert_eq!(text, expected("Design", "Alex: lunch?"));
    }

    #[test]
    fn without_the_text_it_says_who_wrote_in_the_users_language() {
      let direct = notification_text(
        &preview(true, Some("Alex"), None, PreviewContent::Unavailable),
        &german(),
      );
      assert_eq!(direct, expected("Alex", "Neue Nachricht von Alex"));
      let channel = notification_text(
        &preview(false, Some("Alex"), Some("Design"), PreviewContent::Unavailable),
        &german(),
      );
      assert_eq!(channel, expected("Design", "Alex: Neue Nachricht von Alex"));
    }

    #[test]
    fn a_generic_preview_is_never_an_empty_notification() {
      let generic = notification_text(
        &preview(false, None, None, PreviewContent::Unavailable),
        &german(),
      );
      assert_eq!(generic, expected(FALLBACK_TITLE, "Neue Nachricht"));
      assert_eq!(german().generic(), expected(FALLBACK_TITLE, "Neue Nachricht"));
    }

    #[test]
    fn media_is_labelled_by_its_kind_and_other_files_by_name() {
      let photo = file("IMG_1.jpg", AttachmentKind::Image);
      let text = notification_text(&preview(true, Some("Alex"), None, photo), &german());
      assert_eq!(text.body, "Foto");
      let pdf = file("plan.pdf", AttachmentKind::Other);
      assert_eq!(
        notification_text(&preview(true, Some("Alex"), None, pdf), &german()).body,
        "plan.pdf"
      );
    }
  }

  mod calls {
    use super::*;

    #[test]
    fn a_ring_names_the_caller() {
      let mut call = preview(true, Some("Alex"), None, PreviewContent::Unavailable);
      call.wake_up.kind = PushKind::Call;
      assert_eq!(
        notification_text(&call, &german()),
        expected("Eingehender Anruf", "Eingehender Anruf von Alex")
      );
      call.maybe_sender_name = None;
      assert_eq!(notification_text(&call, &german()).body, "Jemand ruft dich an");
    }
  }

  mod labels {
    use super::*;

    #[test]
    fn missing_or_broken_labels_fall_back_to_english() {
      let none = NotificationLabels::from_json("not json");
      assert_eq!(none.generic(), expected(FALLBACK_TITLE, "New message"));
      let no_placeholder = NotificationLabels {
        maybe_new_message_from: Some("Neue Nachricht".into()),
        ..NotificationLabels::default()
      };
      let direct = notification_text(
        &preview(true, Some("Alex"), None, PreviewContent::Unavailable),
        &no_placeholder,
      );
      assert_eq!(direct.body, "New message from Alex");
    }

    #[test]
    fn labels_read_as_the_page_sends_them() {
      let json = r#"{"new_message":"Neue Nachricht","mark_read":"Als gelesen markieren"}"#;
      let labels = NotificationLabels::from_json(json);
      assert_eq!(labels.maybe_new_message.as_deref(), Some("Neue Nachricht"));
      assert_eq!(labels.maybe_mark_read.as_deref(), Some("Als gelesen markieren"));
      assert!(labels.maybe_photo.is_none());
    }
  }

  fn german() -> NotificationLabels {
    NotificationLabels {
      maybe_new_message: Some("Neue Nachricht".into()),
      maybe_new_message_from: Some("Neue Nachricht von {name}".into()),
      maybe_photo: Some("Foto".into()),
      maybe_video: Some("Video".into()),
      maybe_audio: Some("Audio".into()),
      maybe_call_title: Some("Eingehender Anruf".into()),
      maybe_call_from: Some("Eingehender Anruf von {name}".into()),
      maybe_call_unknown: Some("Jemand ruft dich an".into()),
      maybe_test_body: Some("Testbenachrichtigung".into()),
      maybe_mark_read: Some("Als gelesen markieren".into()),
    }
  }

  fn text(text: &str) -> PreviewContent {
    PreviewContent::Text(text.into())
  }

  fn file(name: &str, kind: AttachmentKind) -> PreviewContent {
    PreviewContent::File {
      file_name: name.into(),
      kind,
    }
  }

  fn expected(title: &str, body: &str) -> NotificationText {
    NotificationText {
      title: title.into(),
      body: body.into(),
    }
  }

  fn preview(
    is_direct: bool,
    maybe_sender: Option<&str>,
    maybe_conversation: Option<&str>,
    content: PreviewContent,
  ) -> PushPreview {
    PushPreview {
      wake_up: PushWakeUp {
        kind: PushKind::Message,
        conversation_id: ConversationId::from_snowflake(Snowflake::new(1).unwrap()),
        maybe_message_id: None,
        maybe_sender_id: None,
        is_urgent: false,
        maybe_thread_id: None,
        maybe_call_id: None,
        maybe_badge: None,
        maybe_sent_at: None,
      },
      maybe_conversation_name: maybe_conversation.map(str::to_owned),
      maybe_sender_name: maybe_sender.map(str::to_owned),
      content,
      is_direct,
      is_silent: false,
      is_conversation_verified: true,
      maybe_gap: None,
    }
  }
}
