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
  /// The message whose read position covers this entry: the mention or reply itself, the reaction
  /// message. Reading the conversation (or, for `maybe_thread`, the thread) up to it reads the
  /// entry. Absent from older cores.
  #[serde(default)]
  pub maybe_read_through: Option<MessageId>,
  /// Derived from the read position of the entry's target; there is no flag of its own.
  pub is_unread: bool,
}

/// A workspace admin asks to join a private channel the user owns (admin.md §2.4,
/// `admin_self_join = owner_approval`). It waits above the entries until an owner decides, the
/// admin withdraws it or it expires; it has no read state of its own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
  pub conversation: ConversationId,
  pub user: UserId,
  /// The admin's name as the server gave it: they may share nothing else with the user yet.
  pub user_name: String,
  /// Their username, so two people with one name differ; absent from older cores.
  #[serde(rename = "username", default, skip_serializing_if = "Option::is_none")]
  pub maybe_username: Option<String>,
  pub requested_at: TimestampMs,
  /// When the server closes the request (its clock).
  pub expires_at: TimestampMs,
  pub standing: JoinRequestStanding,
}

/// What the owner can do with a request, as this device's verified roster says.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinRequestStanding {
  /// The fresh signed roster lists the person as an admin: approving adds them to the channel.
  Approvable,
  /// Only the server calls the person an admin (or the roster is too old to trust): the request
  /// can only be declined (admin.md §2.3).
  #[serde(other)]
  NotAnAdmin,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityFeed {
  /// Newest first.
  pub items: Vec<ActivityItem>,
  pub unread_count: u32,
  /// Admins' requests to join channels the user owns, oldest first; absent from older cores.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub join_requests: Vec<JoinRequest>,
}

impl ActivityFeed {
  /// The feed for `items` (any order) with the unread count.
  pub fn from_items(mut items: Vec<ActivityItem>) -> Self {
    items.sort_by(|left, right| right.at.cmp(&left.at).then_with(|| left.id.cmp(&right.id)));
    let unread_count = u32::try_from(items.iter().filter(|item| item.is_unread).count()).unwrap_or(u32::MAX);
    Self {
      items,
      unread_count,
      join_requests: Vec::new(),
    }
  }

  /// The feed with the join requests that wait for the user, oldest first.
  #[must_use]
  pub fn with_join_requests(mut self, mut requests: Vec<JoinRequest>) -> Self {
    requests.sort_by_key(|request| (request.requested_at, request.conversation, request.user));
    self.join_requests = requests;
    self
  }

  /// What the Activity badge counts: unread entries and the requests waiting for a decision.
  pub fn badge_count(&self) -> u32 {
    let waiting = u32::try_from(self.join_requests.len()).unwrap_or(u32::MAX);
    self.unread_count.saturating_add(waiting)
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
      maybe_read_through: None,
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
  fn waiting_join_requests_count_on_the_badge_but_not_as_unread() {
    let request = JoinRequest {
      conversation: id_from_raw(7),
      user: id_from_raw(2),
      user_name: "Admin A".into(),
      maybe_username: Some("admin.a".into()),
      requested_at: 5,
      expires_at: 50,
      standing: JoinRequestStanding::Approvable,
    };
    let feed = ActivityFeed::from_items(vec![item("a", 10, true)]).with_join_requests(vec![request]);
    assert_eq!(feed.unread_count, 1, "mark as read leaves requests alone");
    assert_eq!(feed.badge_count(), 2);
    // An older core sends no requests; a newer standing reads as the cautious one.
    let old: ActivityFeed = serde_json::from_str(r#"{"items":[],"unread_count":0}"#).unwrap();
    assert!(old.join_requests.is_empty());
    assert_eq!(
      serde_json::from_str::<JoinRequestStanding>(r#""escalated""#).unwrap(),
      JoinRequestStanding::NotAnAdmin
    );
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
