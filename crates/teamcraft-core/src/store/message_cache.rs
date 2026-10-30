//! Decoded messages of the conversations read last, kept in memory by [`super::TrackedStore`] and
//! updated with every write it passes on (issue #1000). The UI reads a conversation's messages many
//! times per change (the snapshot entry, the feed window, the activity feed); on SQLCipher each read
//! decrypted and authenticated every page of the conversation and decoded every record, which for a
//! channel of 27,000 messages cost 75 ms a read and kept a core busy while messages came in.
//!
//! Each conversation also has a revision that moves with every write to its messages, so readers can
//! keep what they derived from them ([`super::LocalStore::message_revision`]).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use teamcraft_proto::{ClientMessageId, ConversationId};

use super::StoredMessage;

/// What the cache holds and how much it may hold.
#[derive(Debug, Default)]
pub(crate) struct MessageCache {
  /// Messages kept in all conversations together; `0` keeps none (revisions are still counted).
  capacity: usize,
  /// Shared with readers ([`Self::shared`]); a write while one still reads copies it first.
  conversations: HashMap<ConversationId, Arc<Vec<StoredMessage>>>,
  /// Least recently read first.
  recency: VecDeque<ConversationId>,
  held: usize,
  revisions: HashMap<ConversationId, u64>,
  /// Raised when the disk may differ from every revision handed out (a group of writes rolled
  /// back): part of each revision, so nothing derived before still matches.
  epoch: u64,
}

impl MessageCache {
  pub(crate) fn new(capacity: usize) -> Self {
    Self {
      capacity,
      ..Self::default()
    }
  }

  pub(crate) fn is_enabled(&self) -> bool {
    self.capacity > 0
  }

  pub(crate) fn revision(&self, conversation: ConversationId) -> u64 {
    let writes = self.revisions.get(&conversation).copied().unwrap_or(0);
    (self.epoch << 32) | (writes & u64::from(u32::MAX))
  }

  /// All of the conversation's messages in display order, if it is held, without a copy.
  pub(crate) fn shared(&mut self, conversation: ConversationId) -> Option<Arc<Vec<StoredMessage>>> {
    let messages = Arc::clone(self.conversations.get(&conversation)?);
    self.touch(conversation);
    Some(messages)
  }

  /// Keeps `messages` (all of the conversation, in display order) if nothing was written to the
  /// conversation since `revision`, when it was read, and they fit.
  pub(crate) fn fill(&mut self, conversation: ConversationId, revision: u64, messages: Vec<StoredMessage>) {
    if self.revision(conversation) != revision || messages.len() > self.capacity {
      return;
    }
    self.drop_conversation(conversation);
    self.held += messages.len();
    self.conversations.insert(conversation, Arc::new(messages));
    self.recency.push_back(conversation);
    while self.held > self.capacity
      && let Some(oldest) = self.recency.pop_front()
    {
      if let Some(dropped) = self.conversations.remove(&oldest) {
        self.held -= dropped.len();
      }
    }
  }

  /// A message was written: it replaces the one with its ID, at its place in the order.
  pub(crate) fn upsert(&mut self, message: &StoredMessage) {
    let conversation = message.conversation_id;
    self.bump(conversation);
    let Some(shared) = self.conversations.get_mut(&conversation) else {
      return;
    };
    let messages = Arc::make_mut(shared);
    let had = remove_from(messages, message.client_message_id);
    // After the ones with the same key, like a new row in the database's order.
    let key = message.sort_key();
    let at = messages.partition_point(|held| held.sort_key() <= key);
    messages.insert(at, message.clone());
    if !had {
      self.held += 1;
    }
  }

  pub(crate) fn remove(&mut self, conversation: ConversationId, client_message_id: ClientMessageId) {
    self.bump(conversation);
    if let Some(shared) = self.conversations.get_mut(&conversation)
      && remove_from(Arc::make_mut(shared), client_message_id)
    {
      self.held -= 1;
    }
  }

  pub(crate) fn remove_conversation(&mut self, conversation: ConversationId) {
    self.bump(conversation);
    self.drop_conversation(conversation);
  }

  /// The disk may no longer hold what was written (a group rolled back): nothing held or derived
  /// counts any more.
  pub(crate) fn forget_all(&mut self) {
    self.conversations.clear();
    self.recency.clear();
    self.held = 0;
    self.revisions.clear();
    self.epoch += 1;
  }

  fn bump(&mut self, conversation: ConversationId) {
    let writes = self.revisions.entry(conversation).or_default();
    *writes = writes.wrapping_add(1);
  }

  fn touch(&mut self, conversation: ConversationId) {
    if self.recency.back() != Some(&conversation) {
      self.recency.retain(|held| *held != conversation);
      self.recency.push_back(conversation);
    }
  }

  fn drop_conversation(&mut self, conversation: ConversationId) {
    if let Some(dropped) = self.conversations.remove(&conversation) {
      self.held -= dropped.len();
      self.recency.retain(|held| *held != conversation);
    }
  }
}

/// Removes the message with that ID; whether there was one.
fn remove_from(messages: &mut Vec<StoredMessage>, client_message_id: ClientMessageId) -> bool {
  match messages
    .iter()
    .position(|held| held.client_message_id == client_message_id)
  {
    Some(at) => {
      messages.remove(at);
      true
    },
    None => false,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn a_failed_group_drops_everything_and_every_revision_handed_out() {
    let mut cache = MessageCache::new(10);
    let conversation = id(7);
    let revision = cache.revision(conversation);
    cache.fill(conversation, revision, Vec::new());
    assert!(cache.shared(conversation).is_some());

    cache.forget_all();
    assert!(cache.shared(conversation).is_none());
    assert_ne!(cache.revision(conversation), revision);
  }

  #[test]
  fn a_read_that_a_write_overtook_is_not_kept() {
    let mut cache = MessageCache::new(10);
    let conversation = id(7);
    let revision = cache.revision(conversation);
    cache.remove(conversation, ClientMessageId::from_bytes([1; 16]));
    cache.fill(conversation, revision, Vec::new());
    assert!(cache.shared(conversation).is_none());
  }

  #[test]
  fn the_least_recently_read_conversation_goes_first() {
    let mut cache = MessageCache::new(3);
    for (raw, count) in [(1, 1), (2, 1), (3, 1)] {
      let conversation = id(raw);
      let messages = (0..count).map(|n| message(raw, n)).collect();
      cache.fill(conversation, cache.revision(conversation), messages);
    }
    assert!(cache.shared(id(1)).is_some());
    cache.fill(id(4), cache.revision(id(4)), vec![message(4, 0)]);
    assert!(cache.shared(id(2)).is_none(), "the oldest read goes");
    assert!(cache.shared(id(1)).is_some(), "read again, so kept");
  }

  fn message(conversation: i64, client: u8) -> StoredMessage {
    StoredMessage {
      conversation_id: id(conversation),
      client_message_id: ClientMessageId::from_bytes([client; 16]),
      maybe_server_id: Some(id(100 + i64::from(client))),
      sender_user_id: id(1),
      maybe_thread_id: None,
      sent_at_ms: 0,
      content: super::super::MessageContent::Text {
        body: String::new(),
        mentions: Vec::new(),
        meta: Default::default(),
      },
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
