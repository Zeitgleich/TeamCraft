//! Server-reported state next to the encrypted chat (unread counts, followed threads, presence, read
//! receipts) as a client core keeps it current from gateway events (issue #183), so the UI reads it
//! instead of polling the server for it.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, MessageId, ThreadSummary, UnreadResponse, UserId, UserPresence};

/// What the core knows right now. The UI reads it on every change the core reports.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveSnapshot {
  /// Unread and mention counts; `None` until the first sync answered.
  #[serde(rename = "unread", default, skip_serializing_if = "Option::is_none")]
  pub maybe_unread: Option<UnreadResponse>,
  /// Followed threads with the server's counters.
  #[serde(default)]
  pub threads: Vec<ThreadSummary>,
  /// Presence and custom status as last announced by `presence_update` events.
  #[serde(default)]
  pub presence: Vec<UserPresence>,
  /// How far other members of DMs have read, from `read_receipt` events.
  #[serde(default)]
  pub receipts: Vec<LiveReceipt>,
  #[serde(default)]
  pub marks: LiveMarks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveReceipt {
  pub conversation: ConversationId,
  pub user: UserId,
  pub message: MessageId,
}

/// Counters that move when the UI should read something from the server once: after each gateway
/// connection (events in between may be lost) and when the announcement changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveMarks {
  pub connections: u64,
  pub announcements: u64,
}
