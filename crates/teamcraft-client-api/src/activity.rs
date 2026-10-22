//! The activity feed: what happened to the user since they last looked. The client core builds it
//! from decrypted local data (the server never sees content, so it can't), the UI shows it in one
//! list with an unread state and a jump to the message.

use serde::{Deserialize, Serialize};

use crate::model::{ConversationId, MessageId, ThreadId, TimestampMs, UserId};

/// Why an entry is in the feed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivityKind {
  /// Somebody mentioned the user in a message (also in a thread reply).
  Mention,
  /// Somebody reacted to one of the user's messages.
  Reaction { emoji: String },
  /// Somebody replied in a thread the user started or took part in.
  ThreadReply,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityItem {
  /// Stable across rebuilds of the feed (the UI keys its list by it).
  pub id: String,
  pub kind: ActivityKind,
  pub conversation: ConversationId,
  /// The message to jump to: the mentioning message, the reply, or the message reacted to.
  pub message: MessageId,
  /// Set when `message` is a thread reply: the thread it belongs to.
  #[serde(default)]
  pub maybe_thread: Option<ThreadId>,
  /// Who mentioned, reacted or replied.
  pub actor: UserId,
  pub at: TimestampMs,
  /// The start of the message text (of the reply or mention; of the message reacted to).
  pub excerpt: String,
  pub is_unread: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityFeed {
  /// Newest first.
  pub items: Vec<ActivityItem>,
  pub unread_count: u32,
}

impl ActivityFeed {
  /// The feed for `items` (any order) with the unread count.
  pub fn from_items(mut items: Vec<ActivityItem>) -> Self {
    items.sort_by(|left, right| right.at.cmp(&left.at).then_with(|| left.id.cmp(&right.id)));
    let unread_count = u32::try_from(items.iter().filter(|item| item.is_unread).count()).unwrap_or(u32::MAX);
    Self { items, unread_count }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  fn item(id: &str, at: TimestampMs, is_unread: bool) -> ActivityItem {
    ActivityItem {
      id: id.into(),
      kind: ActivityKind::Mention,
      conversation: id_from_raw(1),
      message: id_from_raw(2),
      maybe_thread: None,
      actor: id_from_raw(3),
      at,
      excerpt: String::new(),
      is_unread,
    }
  }

  #[test]
  fn the_feed_is_newest_first_and_counts_unread() {
    let feed = ActivityFeed::from_items(vec![
      item("a", 10, false),
      item("b", 30, true),
      item("c", 20, true),
    ]);
    let order: Vec<&str> = feed.items.iter().map(|item| item.id.as_str()).collect();
    assert_eq!(order, ["b", "c", "a"]);
    assert_eq!(feed.unread_count, 2);
  }

  #[test]
  fn kinds_are_tagged_json() {
    let json = serde_json::to_string(&ActivityKind::Reaction { emoji: "👍".into() }).unwrap();
    assert_eq!(json, r#"{"kind":"reaction","emoji":"👍"}"#);
    assert_eq!(
      serde_json::from_str::<ActivityKind>(r#"{"kind":"thread_reply"}"#).unwrap(),
      ActivityKind::ThreadReply
    );
  }
}
