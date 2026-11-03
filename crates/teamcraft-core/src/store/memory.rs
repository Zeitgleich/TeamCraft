//! In-memory [`LocalStore`]: tests, and the browser until a persistent web store exists
//! (IndexedDB with an encryption layer; see the crate README).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use teamcraft_proto::{ClientMessageId, ConversationId, MessageId, ThreadId};
use zeroize::Zeroize as _;

use super::{
  LocalStore, MediaRange, MessageFilter, MessageOrder, StoreError, StoredMessage, Table, pick_media,
};

#[derive(Debug, Default)]
pub struct MemoryStore {
  inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
  tables: BTreeMap<(Table, String), Vec<u8>>,
  messages: BTreeMap<(ConversationId, [u8; 16]), StoredMessage>,
  /// Client message ID of each message the server confirmed, by its server ID.
  by_server_id: BTreeMap<(ConversationId, MessageId), [u8; 16]>,
}

impl MemoryStore {
  pub fn new() -> Self {
    Self::default()
  }

  /// The messages of `conversation` that `keep` picks, in display order.
  fn picked(
    &self,
    conversation: ConversationId,
    keep: impl Fn(&StoredMessage) -> bool,
  ) -> Vec<StoredMessage> {
    let mut picked: Vec<StoredMessage> = self
      .lock()
      .messages
      .values()
      .filter(|message| message.conversation_id == conversation && keep(message))
      .cloned()
      .collect();
    picked.sort_by_key(StoredMessage::order);
    picked
  }

  fn lock(&self) -> MutexGuard<'_, Inner> {
    // A poisoned lock only means another thread panicked mid-update; the maps stay usable.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl LocalStore for MemoryStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    Ok(self.lock().tables.get(&(table, key.to_string())).cloned())
  }

  /// The value replaced is overwritten: the cache of a sealed store holds the MLS storage, whose
  /// deleted key packages must not stay in freed memory (issue #391).
  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    if let Some(mut old) = self
      .lock()
      .tables
      .insert((table, key.to_string()), value.to_vec())
    {
      old.zeroize();
    }
    Ok(())
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    if let Some(mut old) = self.lock().tables.remove(&(table, key.to_string())) {
      old.zeroize();
    }
    Ok(())
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    Ok(
      self
        .lock()
        .tables
        .iter()
        .filter(|((entry_table, _), _)| *entry_table == table)
        .map(|((_, key), value)| (key.clone(), value.clone()))
        .collect(),
    )
  }

  fn list_prefix(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    Ok(
      self
        .lock()
        .tables
        .range((table, prefix.to_string())..)
        .take_while(|((entry_table, key), _)| *entry_table == table && key.starts_with(prefix))
        .map(|((_, key), value)| (key.clone(), value.clone()))
        .collect(),
    )
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    let key = (message.conversation_id, *message.client_message_id.as_bytes());
    let mut inner = self.lock();
    if let Some(server_id) = message.maybe_server_id {
      inner
        .by_server_id
        .insert((message.conversation_id, server_id), key.1);
    }
    inner.messages.insert(key, message.clone());
    Ok(())
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    Ok(
      self
        .lock()
        .messages
        .get(&(conversation, *client_message_id.as_bytes()))
        .cloned(),
    )
  }

  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    let inner = self.lock();
    let Some(client_message_id) = inner.by_server_id.get(&(conversation, server_id)) else {
      return Ok(None);
    };
    Ok(inner.messages.get(&(conversation, *client_message_id)).cloned())
  }

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let mut messages: Vec<StoredMessage> = self
      .lock()
      .messages
      .values()
      .filter(|message| message.conversation_id == conversation && message.matches(filter))
      .cloned()
      .collect();
    messages.sort_by_key(StoredMessage::sort_key);
    Ok(messages)
  }

  fn media_messages(
    &self,
    conversation: ConversationId,
    range: MediaRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    // Only the messages with media are copied out of the map.
    let media: Vec<StoredMessage> = self
      .lock()
      .messages
      .values()
      .filter(|message| message.conversation_id == conversation && message.has_media())
      .cloned()
      .collect();
    let mut media = media;
    media.sort_by_key(StoredMessage::sort_key);
    Ok(pick_media(media, range, limit))
  }

  // The feed reads copy only what they return. `indexes_feed` stays false: every read still walks
  // the conversation, which a reader that keeps what it derived (the UI's feed view) does once.
  fn feed_len(&self, conversation: ConversationId) -> Result<usize, StoreError> {
    Ok(
      self
        .lock()
        .messages
        .values()
        .filter(|message| message.conversation_id == conversation && message.is_feed_row())
        .count(),
    )
  }

  fn feed_rows(
    &self,
    conversation: ConversationId,
    start: usize,
    len: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let inner = self.lock();
    let mut rows: Vec<&StoredMessage> = inner
      .messages
      .values()
      .filter(|message| message.conversation_id == conversation && message.is_feed_row())
      .collect();
    rows.sort_by_key(|message| message.order());
    Ok(rows.into_iter().skip(start).take(len).cloned().collect())
  }

  fn feed_ranks(
    &self,
    conversation: ConversationId,
    orders: &[MessageOrder],
  ) -> Result<Vec<usize>, StoreError> {
    let mut rows: Vec<MessageOrder> = self
      .lock()
      .messages
      .values()
      .filter(|message| message.conversation_id == conversation && message.is_feed_row())
      .map(StoredMessage::order)
      .collect();
    rows.sort_unstable();
    Ok(
      orders
        .iter()
        .map(|order| rows.partition_point(|row| row < order))
        .collect(),
    )
  }

  fn event_messages(&self, conversation: ConversationId) -> Result<Vec<StoredMessage>, StoreError> {
    Ok(self.picked(conversation, StoredMessage::is_event))
  }

  fn reactions_to(
    &self,
    conversation: ConversationId,
    targets: &[ClientMessageId],
  ) -> Result<Vec<StoredMessage>, StoreError> {
    Ok(self.picked(conversation, |message| {
      message
        .reaction_target()
        .is_some_and(|target| targets.contains(&target))
    }))
  }

  fn reply_counts(
    &self,
    conversation: ConversationId,
    threads: &[ThreadId],
  ) -> Result<Vec<usize>, StoreError> {
    let inner = self.lock();
    let replies = inner
      .messages
      .values()
      .filter(|message| message.conversation_id == conversation && message.counts_as_reply());
    let mut counts = vec![0; threads.len()];
    for reply in replies {
      if let Some(at) = threads
        .iter()
        .position(|thread| Some(*thread) == reply.maybe_thread_id)
      {
        counts[at] += 1;
      }
    }
    Ok(counts)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    let mut inner = self.lock();
    inner
      .messages
      .retain(|(entry_conversation, _), _| *entry_conversation != conversation);
    inner
      .by_server_id
      .retain(|(entry_conversation, _), _| *entry_conversation != conversation);
    Ok(())
  }

  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let key = *client_message_id.as_bytes();
    inner.messages.remove(&(conversation, key));
    inner
      .by_server_id
      .retain(|(entry_conversation, _), entry| *entry_conversation != conversation || *entry != key);
    Ok(())
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::tests::exercise_store;

  #[test]
  fn memory_store_behaves_like_a_store() {
    exercise_store(&MemoryStore::new());
  }
}
