//! In-memory [`LocalStore`]: tests, and the browser until a persistent web store exists
//! (IndexedDB with an encryption layer; see the crate README).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use teamcraft_proto::{ClientMessageId, ConversationId, MessageId};

use super::{LocalStore, MessageFilter, StoreError, StoredMessage, Table};

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

  fn lock(&self) -> MutexGuard<'_, Inner> {
    // A poisoned lock only means another thread panicked mid-update; the maps stay usable.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl LocalStore for MemoryStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    Ok(self.lock().tables.get(&(table, key.to_string())).cloned())
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    self
      .lock()
      .tables
      .insert((table, key.to_string()), value.to_vec());
    Ok(())
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.lock().tables.remove(&(table, key.to_string()));
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
