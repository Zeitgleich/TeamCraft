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

use super::{StoredMessage, WriteTally};

/// What the cache holds and how much it may hold.
#[derive(Debug, Default)]
pub(crate) struct MessageCache {
  /// Bytes of messages kept in all conversations together ([`StoredMessage::footprint_bytes`]);
  /// `0` keeps none (revisions are still counted).
  capacity_bytes: usize,
  conversations: HashMap<ConversationId, Held>,
  /// Least recently read first.
  recency: VecDeque<ConversationId>,
  /// The summed [`Held::bytes`].
  held_bytes: usize,
  revisions: HashMap<ConversationId, u64>,
  /// Raised when the disk may differ from every revision handed out (a group of writes rolled
  /// back): part of each revision, so nothing derived before still matches.
  epoch: u64,
  /// Commits of other connections seen so far ([`super::WriteTally::foreign`]).
  seen_foreign: u64,
}

/// One cached conversation.
#[derive(Debug)]
struct Held {
  /// Shared with readers ([`MessageCache::shared`]); a write while one still reads copies it first.
  /// Dropping it from the cache never takes it from a reader.
  messages: Arc<Vec<StoredMessage>>,
  /// The summed footprint of `messages`, kept up to date by every write.
  bytes: usize,
}

impl MessageCache {
  pub(crate) fn new(capacity_bytes: usize) -> Self {
    Self {
      capacity_bytes,
      ..Self::default()
    }
  }

  pub(crate) fn is_enabled(&self) -> bool {
    self.capacity_bytes > 0
  }

  pub(crate) fn revision(&self, conversation: ConversationId) -> u64 {
    let writes = self.revisions.get(&conversation).copied().unwrap_or(0);
    (self.epoch << 32) | (writes & u64::from(u32::MAX))
  }

  /// All of the conversation's messages in display order, if it is held, without a copy.
  pub(crate) fn shared(&mut self, conversation: ConversationId) -> Option<Arc<Vec<StoredMessage>>> {
    let messages = Arc::clone(&self.conversations.get(&conversation)?.messages);
    self.touch(conversation);
    Some(messages)
  }

  /// Keeps `messages` (all of the conversation, in display order) if nothing was written to the
  /// conversation since `revision`, when it was read, and they fit. The one filled becomes the
  /// most recently read; the least recently read go to make room.
  pub(crate) fn fill(&mut self, conversation: ConversationId, revision: u64, messages: Vec<StoredMessage>) {
    if self.revision(conversation) != revision {
      return;
    }
    let bytes = messages.iter().map(StoredMessage::footprint_bytes).sum::<usize>();
    if bytes > self.capacity_bytes {
      return;
    }
    self.drop_conversation(conversation);
    self.held_bytes += bytes;
    self.conversations.insert(
      conversation,
      Held {
        messages: Arc::new(messages),
        bytes,
      },
    );
    self.recency.push_back(conversation);
    self.evict_others(conversation);
  }

  /// A message was written: it replaces the one with its ID, at its place in the order.
  pub(crate) fn upsert(&mut self, message: &StoredMessage) {
    let conversation = message.conversation_id;
    self.bump(conversation);
    let Some(held) = self.conversations.get_mut(&conversation) else {
      return;
    };
    let messages = Arc::make_mut(&mut held.messages);
    let maybe_replaced = remove_from(messages, message.client_message_id);
    // After the ones with the same key, like a new row in the database's order.
    let key = message.sort_key();
    let at = messages.partition_point(|held| held.sort_key() <= key);
    messages.insert(at, message.clone());
    let added = message.footprint_bytes();
    let removed = maybe_replaced.map_or(0, |old| old.footprint_bytes());
    held.bytes = held.bytes - removed + added;
    self.held_bytes = self.held_bytes - removed + added;
    if self.held_bytes > self.capacity_bytes {
      self.make_room_after_growth(conversation);
    }
  }

  pub(crate) fn remove(&mut self, conversation: ConversationId, client_message_id: ClientMessageId) {
    self.bump(conversation);
    if let Some(held) = self.conversations.get_mut(&conversation)
      && let Some(removed) = remove_from(Arc::make_mut(&mut held.messages), client_message_id)
    {
      let bytes = removed.footprint_bytes();
      held.bytes -= bytes;
      self.held_bytes -= bytes;
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
    self.held_bytes = 0;
    self.revisions.clear();
    self.epoch += 1;
  }

  /// Drops everything if another connection committed since the last look: it may have changed any
  /// conversation, and which one is unknown. `None`: the store has no other writer.
  pub(crate) fn follow_disk(&mut self, maybe_tally: Option<WriteTally>) {
    let Some(tally) = maybe_tally else {
      return;
    };
    if tally.foreign != self.seen_foreign {
      self.seen_foreign = tally.foreign;
      self.forget_all();
    }
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

  /// A write made the conversation bigger. The least recently read go first, but the written
  /// conversation stays if it fits alone: it is the one being worked in. One that outgrew the whole
  /// cache is dropped (the next read finds the revision moved and reads it from the store).
  fn make_room_after_growth(&mut self, conversation: ConversationId) {
    self.evict_others(conversation);
    if self.held_bytes > self.capacity_bytes {
      self.drop_conversation(conversation);
    }
  }

  /// Drops the least recently read conversations other than `keep` until the rest fit.
  fn evict_others(&mut self, keep: ConversationId) {
    while self.held_bytes > self.capacity_bytes {
      let Some(at) = self.recency.iter().position(|held| *held != keep) else {
        return;
      };
      if let Some(oldest) = self.recency.remove(at) {
        self.release(oldest);
      }
    }
  }

  fn drop_conversation(&mut self, conversation: ConversationId) {
    if self.release(conversation) {
      self.recency.retain(|held| *held != conversation);
    }
  }

  /// Takes the conversation out of the map and the byte count, not out of the recency list.
  fn release(&mut self, conversation: ConversationId) -> bool {
    match self.conversations.remove(&conversation) {
      Some(dropped) => {
        self.held_bytes -= dropped.bytes;
        true
      },
      None => false,
    }
  }
}

/// Removes the message with that ID and gives it back, if there was one.
fn remove_from(
  messages: &mut Vec<StoredMessage>,
  client_message_id: ClientMessageId,
) -> Option<StoredMessage> {
  let at = messages
    .iter()
    .position(|held| held.client_message_id == client_message_id)?;
  Some(messages.remove(at))
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  /// Room for any few small messages.
  const ROOM: usize = 100_000;

  #[test]
  fn a_failed_group_drops_everything_and_every_revision_handed_out() {
    let mut cache = MessageCache::new(ROOM);
    let conversation = id(7);
    let revision = cache.revision(conversation);
    cache.fill(conversation, revision, Vec::new());
    assert!(cache.shared(conversation).is_some());

    cache.forget_all();
    assert!(cache.shared(conversation).is_none());
    assert_ne!(cache.revision(conversation), revision);
  }

  #[test]
  fn another_connections_commit_drops_everything_once() {
    let mut cache = MessageCache::new(ROOM);
    let conversation = id(7);
    cache.follow_disk(Some(WriteTally::default()));
    cache.fill(conversation, cache.revision(conversation), Vec::new());
    let revision = cache.revision(conversation);

    cache.follow_disk(None);
    cache.follow_disk(Some(WriteTally { lost: 3, foreign: 0 }));
    assert!(
      cache.shared(conversation).is_some(),
      "lost writes are the group's business"
    );

    let foreign = Some(WriteTally { lost: 0, foreign: 1 });
    cache.follow_disk(foreign);
    assert!(cache.shared(conversation).is_none());
    assert_ne!(cache.revision(conversation), revision);

    cache.fill(conversation, cache.revision(conversation), Vec::new());
    cache.follow_disk(foreign);
    assert!(cache.shared(conversation).is_some(), "seen already");
  }

  #[test]
  fn a_read_that_a_write_overtook_is_not_kept() {
    let mut cache = MessageCache::new(ROOM);
    let conversation = id(7);
    let revision = cache.revision(conversation);
    cache.remove(conversation, ClientMessageId::from_bytes([1; 16]));
    cache.fill(conversation, revision, Vec::new());
    assert!(cache.shared(conversation).is_none());
  }

  #[test]
  fn the_least_recently_read_conversation_goes_first() {
    let one = message(1, 0).footprint_bytes();
    let mut cache = MessageCache::new(3 * one);
    for raw in [1, 2, 3] {
      cache.fill(id(raw), cache.revision(id(raw)), vec![message(raw, 0)]);
    }
    assert!(cache.shared(id(1)).is_some());
    cache.fill(id(4), cache.revision(id(4)), vec![message(4, 0)]);
    assert!(cache.shared(id(2)).is_none(), "the oldest read goes");
    assert!(cache.shared(id(1)).is_some(), "read again, so kept");
  }

  /// Issue #1125: the same number of messages holds very different amounts of memory.
  #[test]
  fn long_messages_fill_the_cache_sooner_than_short_ones() {
    let long = 64 * 1024;
    let mut cache = MessageCache::new(10 * long);
    for raw in 1..=3 {
      let messages = (0..4).map(|n| long_message(raw, n, long)).collect();
      cache.fill(id(raw), cache.revision(id(raw)), messages);
    }
    // 12 long messages are over 10 of them: the first conversation had to go.
    assert!(cache.shared(id(1)).is_none());
    assert!(cache.shared(id(2)).is_some());
    assert!(cache.shared(id(3)).is_some());
    assert!(cache.held_bytes <= cache.capacity_bytes);

    let many_short = (0..200).map(|n| message(9, n)).collect();
    cache.fill(id(9), cache.revision(id(9)), many_short);
    assert!(
      cache.shared(id(9)).is_some(),
      "200 short messages are far less than 10 long ones"
    );

    cache.fill(id(8), cache.revision(id(8)), vec![long_message(8, 0, 11 * long)]);
    assert!(
      cache.shared(id(8)).is_none(),
      "one conversation larger than the cache is not kept"
    );
    assert!(cache.held_bytes <= cache.capacity_bytes);
  }

  #[test]
  fn writes_keep_the_byte_count_and_the_bound() {
    let long = 64 * 1024;
    let mut cache = MessageCache::new(6 * long);
    cache.fill(id(1), cache.revision(id(1)), vec![long_message(1, 0, long)]);
    cache.fill(id(2), cache.revision(id(2)), vec![long_message(2, 0, long)]);
    let sum = |cache: &MessageCache| cache.conversations.values().map(|held| held.bytes).sum::<usize>();
    assert_eq!(cache.held_bytes, sum(&cache));

    // Replacing a message with a longer one and removing another change the figure by the difference.
    cache.upsert(&long_message(1, 0, 2 * long));
    assert_eq!(cache.held_bytes, sum(&cache));
    assert!(cache.held_bytes >= 3 * long);
    cache.remove(id(2), ClientMessageId::from_bytes([0; 16]));
    assert_eq!(cache.held_bytes, sum(&cache));
    assert!(cache.held_bytes < 3 * long);

    // Growing the conversation being written drops the others, never itself, while it fits.
    cache.fill(id(3), cache.revision(id(3)), vec![long_message(3, 0, long)]);
    for n in 1..=3 {
      cache.upsert(&long_message(1, n, long));
      assert!(cache.held_bytes <= cache.capacity_bytes);
      assert_eq!(cache.held_bytes, sum(&cache));
    }
    assert!(cache.shared(id(1)).is_some(), "the conversation written to stays");
    assert!(cache.shared(id(3)).is_none());

    // Past the whole cache it goes too, and its revision has moved so a read-in-flight is refused.
    let revision = cache.revision(id(1));
    cache.upsert(&long_message(1, 9, 7 * long));
    assert!(cache.shared(id(1)).is_none());
    assert_eq!(cache.held_bytes, 0);
    cache.fill(id(1), revision, Vec::new());
    assert!(cache.shared(id(1)).is_none());
  }

  #[test]
  fn what_a_reader_holds_survives_its_eviction() {
    let long = 64 * 1024;
    let mut cache = MessageCache::new(2 * long);
    cache.fill(id(1), cache.revision(id(1)), vec![long_message(1, 0, long)]);
    let reading = cache.shared(id(1)).unwrap();
    cache.fill(id(2), cache.revision(id(2)), vec![long_message(2, 0, long)]);
    cache.fill(id(3), cache.revision(id(3)), vec![long_message(3, 0, long)]);
    assert!(cache.shared(id(1)).is_none());
    assert_eq!(reading.len(), 1, "the reader keeps its copy");
  }

  fn long_message(conversation: i64, client: u8, body_bytes: usize) -> StoredMessage {
    let mut long = message(conversation, client);
    long.content = super::super::MessageContent::Text {
      body: "x".repeat(body_bytes),
      mentions: Vec::new(),
      meta: Default::default(),
    };
    long
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
      maybe_unreadable: None,
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
