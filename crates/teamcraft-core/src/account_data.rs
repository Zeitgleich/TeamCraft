//! The user's personal data that follows them across devices (e2ee.md §6.4): saved items, drafts
//! and saved searches, and the trust decisions (pinned identity keys and the workspace root of
//! trust, security review A2), so a new or linked device doesn't trust the server on first use
//! again. It travels inside the encrypted key bag, so the server sees neither content nor count.
//!
//! Devices change it independently and upload whole key bags, so every entry carries the time of
//! its last change and removals stay as tombstones; merging keeps the newer side per entry. Clocks
//! of one user's devices are close enough for "last edit wins" on personal data.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, ClientMessageId, ConversationId, MessageId, ThreadId, UserId};

/// Tombstones older than this are dropped when merging (every device has synced by then).
pub const TOMBSTONE_LIFETIME_MS: i64 = 90 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountData {
  /// Saved messages, keyed by [`saved_key`].
  #[serde(default)]
  pub saved: BTreeMap<String, Stamped<SavedItem>>,
  /// Unsent text per conversation or thread, keyed by [`draft_key`]; an empty body is "no draft".
  #[serde(default)]
  pub drafts: BTreeMap<String, Stamped<Draft>>,
  /// Saved searches, keyed by their ID (random, so devices don't collide).
  #[serde(default)]
  pub saved_searches: BTreeMap<u64, Stamped<SavedSearchEntry>>,
  /// Conversations the user marked unread, keyed by conversation ID (the server's read position
  /// only moves forward, so the mark is the user's own).
  #[serde(default)]
  pub marked_unread: BTreeMap<String, Stamped<UnreadMark>>,
  /// Identity keys the user's devices pinned, keyed by user ID; an accepted key change is a newer
  /// entry.
  #[serde(default)]
  pub pins: BTreeMap<String, Stamped<PinnedKey>>,
  /// The workspace's first owner, as the first of the user's devices verified it.
  #[serde(rename = "workspace_root", default, skip_serializing_if = "Option::is_none")]
  pub maybe_workspace_root: Option<PinnedKey>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PinnedKey {
  pub user_id: UserId,
  pub public_key: Base64Bytes,
}

/// A value with the time of its last change; `is_removed` marks a deletion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamped<T> {
  pub value: T,
  pub updated_at_ms: i64,
  #[serde(default)]
  pub is_removed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedItem {
  pub conversation_id: ConversationId,
  pub client_message_id: ClientMessageId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
  pub conversation_id: ConversationId,
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreadMark {
  pub conversation_id: ConversationId,
  /// Unread from this message on.
  pub from_message: MessageId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedSearchEntry {
  pub name: String,
  pub query: String,
}

impl AccountData {
  /// Decodes the key bag's account data; an empty field (new or version 1 key bag) is empty data.
  pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
    if bytes.is_empty() {
      Ok(Self::default())
    } else {
      serde_json::from_slice(bytes)
    }
  }

  pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(self)
  }

  /// Takes the newer side of every entry from `other`, then forgets old tombstones.
  pub fn merge(&mut self, other: &Self, now_ms: i64) {
    merge_map(&mut self.saved, &other.saved);
    merge_map(&mut self.drafts, &other.drafts);
    merge_map(&mut self.saved_searches, &other.saved_searches);
    merge_map(&mut self.marked_unread, &other.marked_unread);
    merge_map(&mut self.pins, &other.pins);
    // The root never changes; the first one a device of the user verified stays.
    if self.maybe_workspace_root.is_none() {
      self.maybe_workspace_root = other.maybe_workspace_root.clone();
    }
    self.forget_tombstones(now_ms);
  }

  pub fn forget_tombstones(&mut self, now_ms: i64) {
    let is_fresh = |entry_removed: bool, updated_at_ms: i64| {
      !entry_removed || now_ms.saturating_sub(updated_at_ms) < TOMBSTONE_LIFETIME_MS
    };
    self
      .saved
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
    self
      .drafts
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
    self
      .saved_searches
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
    self
      .marked_unread
      .retain(|_, entry| is_fresh(entry.is_removed, entry.updated_at_ms));
  }

  /// The identity key the user's devices pinned for `user`.
  pub fn pinned_key(&self, user: UserId) -> Option<[u8; 32]> {
    self
      .pins
      .get(&user.to_string())
      .filter(|entry| !entry.is_removed)
      .and_then(|entry| <[u8; 32]>::try_from(entry.value.public_key.as_slice()).ok())
  }

  /// Records a pin; returns whether it changed anything.
  pub fn pin(&mut self, user: UserId, public_key: [u8; 32], now_ms: i64) -> bool {
    if self.pinned_key(user) == Some(public_key) {
      return false;
    }
    self.pins.insert(
      user.to_string(),
      Stamped {
        value: PinnedKey {
          user_id: user,
          public_key: Base64Bytes::new(public_key.to_vec()),
        },
        updated_at_ms: now_ms,
        is_removed: false,
      },
    );
    true
  }

  /// Where the user marked the conversation unread, if they did and haven't read it since.
  pub fn unread_mark(&self, conversation: ConversationId) -> Option<MessageId> {
    self
      .marked_unread
      .get(&conversation.to_string())
      .filter(|entry| !entry.is_removed)
      .map(|entry| entry.value.from_message)
  }

  pub fn mark_unread(&mut self, conversation: ConversationId, from: MessageId, now_ms: i64) {
    self.marked_unread.insert(
      conversation.to_string(),
      Stamped {
        value: UnreadMark {
          conversation_id: conversation,
          from_message: from,
        },
        updated_at_ms: now_ms,
        is_removed: false,
      },
    );
  }

  /// Removes the mark (the user read the conversation); returns whether there was one.
  pub fn clear_unread_mark(&mut self, conversation: ConversationId, now_ms: i64) -> bool {
    match self.marked_unread.get_mut(&conversation.to_string()) {
      Some(entry) if !entry.is_removed => {
        entry.is_removed = true;
        entry.updated_at_ms = now_ms;
        true
      },
      _ => false,
    }
  }

  /// Saved items, newest first.
  pub fn saved_items(&self) -> Vec<(&SavedItem, i64)> {
    let mut items: Vec<(&SavedItem, i64)> = self
      .saved
      .values()
      .filter(|entry| !entry.is_removed)
      .map(|entry| (&entry.value, entry.updated_at_ms))
      .collect();
    items.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
    items
  }

  pub fn is_saved(&self, conversation: ConversationId, message: ClientMessageId) -> bool {
    self
      .saved
      .get(&saved_key(conversation, message))
      .is_some_and(|entry| !entry.is_removed)
  }

  pub fn set_saved(
    &mut self,
    conversation: ConversationId,
    message: ClientMessageId,
    is_saved: bool,
    now_ms: i64,
  ) {
    self.saved.insert(
      saved_key(conversation, message),
      Stamped {
        value: SavedItem {
          conversation_id: conversation,
          client_message_id: message,
        },
        updated_at_ms: now_ms,
        is_removed: !is_saved,
      },
    );
  }

  pub fn draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) -> Option<&str> {
    self
      .drafts
      .get(&draft_key(conversation, maybe_thread))
      .filter(|entry| !entry.is_removed && !entry.value.body.is_empty())
      .map(|entry| entry.value.body.as_str())
  }

  /// Stores (or, with an empty body, clears) the draft; returns whether anything changed.
  pub fn set_draft(
    &mut self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    body: &str,
    now_ms: i64,
  ) -> bool {
    if self.draft(conversation, maybe_thread).unwrap_or_default() == body {
      return false;
    }
    self.drafts.insert(
      draft_key(conversation, maybe_thread),
      Stamped {
        value: Draft {
          conversation_id: conversation,
          maybe_thread_id: maybe_thread,
          body: body.to_string(),
        },
        updated_at_ms: now_ms,
        is_removed: body.is_empty(),
      },
    );
    true
  }

  pub fn saved_searches(&self) -> Vec<(u64, &SavedSearchEntry)> {
    self
      .saved_searches
      .iter()
      .filter(|(_, entry)| !entry.is_removed)
      .map(|(id, entry)| (*id, &entry.value))
      .collect()
  }

  pub fn save_search(&mut self, id: u64, name: &str, query: &str, now_ms: i64) {
    self.saved_searches.insert(
      id,
      Stamped {
        value: SavedSearchEntry {
          name: name.to_string(),
          query: query.to_string(),
        },
        updated_at_ms: now_ms,
        is_removed: false,
      },
    );
  }

  pub fn delete_saved_search(&mut self, id: u64, now_ms: i64) {
    if let Some(entry) = self.saved_searches.get_mut(&id) {
      entry.is_removed = true;
      entry.updated_at_ms = now_ms;
    }
  }
}

pub fn saved_key(conversation: ConversationId, message: ClientMessageId) -> String {
  let hex: String = message
    .as_bytes()
    .iter()
    .map(|byte| format!("{byte:02x}"))
    .collect();
  format!("{conversation}/{hex}")
}

pub fn draft_key(conversation: ConversationId, maybe_thread: Option<ThreadId>) -> String {
  match maybe_thread {
    Some(thread) => format!("{conversation}/{thread}"),
    None => conversation.to_string(),
  }
}

/// Per key, the entry with the later change wins; on a tie the removal wins (it is the safer
/// outcome for personal data and keeps merging symmetric).
fn merge_map<K: Ord + Clone, T: Clone>(
  local: &mut BTreeMap<K, Stamped<T>>,
  remote: &BTreeMap<K, Stamped<T>>,
) {
  for (key, theirs) in remote {
    let is_newer = match local.get(key) {
      None => true,
      Some(ours) => {
        theirs.updated_at_ms > ours.updated_at_ms
          || (theirs.updated_at_ms == ours.updated_at_ms && theirs.is_removed && !ours.is_removed)
      },
    };
    if is_newer {
      local.insert(key.clone(), theirs.clone());
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn newer_changes_win_per_entry_in_both_directions() {
    let (conversation, message) = (id(1), ClientMessageId::from_bytes([7; 16]));
    let mut phone = AccountData::default();
    let mut laptop = AccountData::default();
    phone.set_saved(conversation, message, true, 100);
    laptop.set_draft(conversation, None, "half a thought", 150);
    laptop.set_saved(conversation, message, false, 200);
    phone.set_draft(conversation, None, "a later thought", 300);

    let mut merged_on_phone = phone.clone();
    merged_on_phone.merge(&laptop, 400);
    let mut merged_on_laptop = laptop.clone();
    merged_on_laptop.merge(&phone, 400);

    assert_eq!(merged_on_phone, merged_on_laptop, "merging is symmetric");
    assert!(
      !merged_on_phone.is_saved(conversation, message),
      "the later unsave wins"
    );
    assert_eq!(merged_on_phone.draft(conversation, None), Some("a later thought"));
  }

  #[test]
  fn clearing_a_draft_and_old_tombstones() {
    let conversation = id(1);
    let thread: ThreadId = id(9);
    let mut data = AccountData::default();
    assert!(data.set_draft(conversation, Some(thread), "reply", 10));
    assert!(
      !data.set_draft(conversation, Some(thread), "reply", 11),
      "unchanged"
    );
    assert!(data.set_draft(conversation, Some(thread), "", 12));
    assert_eq!(data.draft(conversation, Some(thread)), None);
    assert_eq!(data.drafts.len(), 1, "kept as a tombstone");
    data.forget_tombstones(12 + TOMBSTONE_LIFETIME_MS);
    assert!(data.drafts.is_empty());
  }

  #[test]
  fn saved_searches_and_encoding() {
    let mut data = AccountData::default();
    data.save_search(5, "Mine", "from:@me", 1);
    data.save_search(6, "Files", "has:file", 2);
    data.delete_saved_search(5, 3);
    assert_eq!(
      data
        .saved_searches()
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>(),
      vec![6]
    );
    let decoded = AccountData::from_bytes(&data.to_bytes().unwrap()).unwrap();
    assert_eq!(decoded, data);
    assert_eq!(AccountData::from_bytes(&[]).unwrap(), AccountData::default());
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
