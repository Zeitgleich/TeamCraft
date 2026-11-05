//! What the "Drafts & sent" view lists (#1220). Both lists are read on the device: drafts from the
//! encrypted account data (e2ee.md §6.4), sent messages from the local store. The server is asked
//! nothing new and learns nothing about what is looked at.

use serde::{Deserialize, Serialize};

use crate::model::{ConversationId, MessageId, ThreadId, TimestampMs};

/// One unsent text, in a conversation or in one of its threads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftItem {
  pub conversation: ConversationId,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread: Option<ThreadId>,
  pub conversation_name: String,
  /// The start of the message the thread hangs on, where this device can read it.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_root: Option<String>,
  /// The draft's source text: formatting and mentions are part of it.
  pub body: String,
  /// When a device of the user last changed it.
  pub edited_at: TimestampMs,
}

/// One message the user sent, as much of it as the list shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentItem {
  pub conversation: ConversationId,
  pub conversation_name: String,
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
