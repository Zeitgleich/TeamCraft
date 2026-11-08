//! What the "Drafts & sent" view lists (#1220). Both lists are read on the device: drafts from the
//! encrypted account data (e2ee.md §6.4), sent messages from the local store. The server is asked
//! nothing new and learns nothing about what is looked at.

use serde::{Deserialize, Serialize};

use teamcraft_proto::AttachmentId;

use crate::model::{ConversationId, MessageId, ThreadId, TimestampMs};

/// One unsent text, in a conversation or in one of its threads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftItem {
  pub conversation: ConversationId,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread: Option<ThreadId>,
  pub conversation_name: String,
  /// The conversation is the user's notes to themself: `conversation_name` is empty, the apps name
  /// it in the user's language (#1385).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_notes_to_self: bool,
  /// The start of the message the thread hangs on, where this device can read it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_root: Option<String>,
  /// The draft's source text: formatting and mentions are part of it.
  pub body: String,
  /// When a device of the user last changed it.
  pub edited_at: TimestampMs,
  /// Files attached to it (#1220); a draft may hold files and no text.
  #[serde(default, skip_serializing_if = "is_zero")]
  pub file_count: u32,
}

/// What a composer's message replies to, kept with the draft on this device (#1115). IDs only: the
/// quote is read from the local store when the composer comes back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftReply {
  pub message: MessageId,
  /// The one picture of an album the reply answers.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_attachment: Option<AttachmentId>,
}

/// A file attached to a draft (#1220), as the composer shows it. Files that went up travel with
/// the draft to the user's other devices, which send them without uploading them again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftFileItem {
  /// Names the file for [`crate::actions::FileSource::Draft`] and for removing it.
  pub id: String,
  pub file_name: String,
  pub media_type: String,
  /// Plain size of the file.
  pub size_bytes: u64,
  /// A small picture of it, where there is one.
  #[serde(rename = "thumbnail", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thumbnail: Option<Vec<u8>>,
  pub state: DraftFileState,
}

/// Where a draft's file stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DraftFileState {
  /// Going up from this device; the draft's other devices see it once it is up.
  Uploading,
  /// Up and in the draft on every device of the user.
  Ready,
  /// Kept on this device until it is sent: the server holds no drafts' files (an older one), or
  /// the drafts already carry as many files as travel.
  OnThisDevice,
  /// Can't go up (unreadable, too large) or gave up retrying: the user removes it.
  Failed,
  /// Held by the server longer than it keeps drafts' files (`draft_file_retention_days`): it is
  /// gone or about to be, and is not sent. The user attaches it again.
  Expired,
  #[serde(other)]
  Unknown,
}

/// One message the user sent, as much of it as the list shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentItem {
  pub conversation: ConversationId,
  pub conversation_name: String,
  /// The conversation is the user's notes to themself: `conversation_name` is empty, the apps name
  /// it in the user's language (#1385).
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub is_notes_to_self: bool,
  pub message: MessageId,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread: Option<ThreadId>,
  /// The start of the text; empty for a message of only files.
  pub excerpt: String,
  #[serde(default, skip_serializing_if = "is_zero")]
  pub file_count: u32,
  pub sent_at: TimestampMs,
}

fn is_zero(count: &u32) -> bool {
  *count == 0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_notes_flag_is_new_for_older_data_and_left_out_when_false() {
    let older: SentItem = serde_json::from_str(
      r#"{"conversation":"7","conversation_name":"general","message":"10","excerpt":"hi","sent_at":900}"#,
    )
    .unwrap();
    assert!(!older.is_notes_to_self);
    assert!(
      !serde_json::to_string(&older)
        .unwrap()
        .contains("is_notes_to_self")
    );

    let notes = SentItem {
      is_notes_to_self: true,
      conversation_name: String::new(),
      ..older
    };
    let json = serde_json::to_string(&notes).unwrap();
    assert!(json.contains(r#""is_notes_to_self":true"#));
    assert_eq!(serde_json::from_str::<SentItem>(&json).unwrap(), notes);
  }
}
