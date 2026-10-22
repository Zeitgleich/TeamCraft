//! Read state the server reports: followed threads, unread counts, other members' read receipts
//! and acknowledgements of urgent messages. The sync engine writes it from gateway events; UIs read
//! it with the functions here.

use std::collections::BTreeMap;

use teamcraft_proto::{ConversationId, MessageId, ThreadId, ThreadSummary, UnreadResponse, UserId};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

const UNREAD_KEY: &str = "state";

/// Followed threads, most recently active first. Last replies are decrypted into the message store
/// separately; `maybe_last_reply` here keeps only the ciphertext the server sent.
pub fn threads(store: &impl LocalStore) -> Result<Vec<ThreadSummary>, CoreError> {
  let mut threads: Vec<ThreadSummary> = records::list_json(store, Table::Threads)?;
  threads.sort_by_key(|thread| std::cmp::Reverse(thread.maybe_last_reply.as_ref().map(|reply| reply.id)));
  Ok(threads)
}

/// Unread and mention counts as last reported by the server.
pub fn unread(store: &impl LocalStore) -> Result<Option<UnreadResponse>, CoreError> {
  records::get_json(store, Table::Unread, UNREAD_KEY)
}

/// How far each other member of a DM has read.
pub fn read_receipts(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<BTreeMap<UserId, MessageId>, CoreError> {
  let prefix = format!("{conversation}/");
  let mut receipts = BTreeMap::new();
  for (key, value) in store.list(Table::ReadReceipts)? {
    let Some(user) = key.strip_prefix(&prefix) else {
      continue;
    };
    let user = user
      .parse()
      .map_err(|_| CoreError::Corrupt(format!("read receipt key {key}")))?;
    let message: MessageId =
      serde_json::from_slice(&value).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    receipts.insert(user, message);
  }
  Ok(receipts)
}

/// Members who acknowledged an urgent message, in ID order.
pub fn acknowledgements(
  store: &impl LocalStore,
  conversation: ConversationId,
  message: MessageId,
) -> Result<Vec<UserId>, CoreError> {
  Ok(records::get_json(store, Table::MessageAcks, &ack_key(conversation, message))?.unwrap_or_default())
}

/// Replaces the stored threads with the server's list.
pub(crate) fn save_threads(store: &impl LocalStore, threads: &[ThreadSummary]) -> Result<(), CoreError> {
  for (key, _) in store.list(Table::Threads)? {
    store.delete(Table::Threads, &key)?;
  }
  for thread in threads {
    records::put_json(
      store,
      Table::Threads,
      &thread_key(thread.conversation_id, thread.thread_id),
      thread,
    )?;
  }
  Ok(())
}

pub(crate) fn save_unread(store: &impl LocalStore, unread: &UnreadResponse) -> Result<(), CoreError> {
  records::put_json(store, Table::Unread, UNREAD_KEY, unread)
}

/// Records a receipt; receipts only move forward (events may arrive out of order).
pub(crate) fn note_read_receipt(
  store: &impl LocalStore,
  conversation: ConversationId,
  user: UserId,
  message: MessageId,
) -> Result<(), CoreError> {
  let key = format!("{conversation}/{user}");
  let maybe_known: Option<MessageId> = records::get_json(store, Table::ReadReceipts, &key)?;
  if maybe_known.is_some_and(|known| known >= message) {
    return Ok(());
  }
  records::put_json(store, Table::ReadReceipts, &key, &message)
}

pub(crate) fn note_acknowledgement(
  store: &impl LocalStore,
  conversation: ConversationId,
  message: MessageId,
  user: UserId,
) -> Result<(), CoreError> {
  let key = ack_key(conversation, message);
  let mut users: Vec<UserId> = records::get_json(store, Table::MessageAcks, &key)?.unwrap_or_default();
  if let Err(position) = users.binary_search(&user) {
    users.insert(position, user);
    records::put_json(store, Table::MessageAcks, &key, &users)?;
  }
  Ok(())
}

fn thread_key(conversation: ConversationId, thread: ThreadId) -> String {
  format!("{conversation}/{thread}")
}

fn ack_key(conversation: ConversationId, message: MessageId) -> String {
  format!("{conversation}/{message}")
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn receipts_only_move_forward_and_are_per_conversation() {
    let store = MemoryStore::new();
    note_read_receipt(&store, id(1), id(7), id(20)).unwrap();
    note_read_receipt(&store, id(1), id(7), id(10)).unwrap();
    note_read_receipt(&store, id(1), id(8), id(15)).unwrap();
    note_read_receipt(&store, id(2), id(7), id(99)).unwrap();
    let receipts = read_receipts(&store, id(1)).unwrap();
    assert_eq!(receipts.get(&id(7)), Some(&id(20)));
    assert_eq!(receipts.get(&id(8)), Some(&id(15)));
    assert_eq!(receipts.len(), 2);
  }

  #[test]
  fn acknowledgements_are_deduplicated_and_sorted() {
    let store = MemoryStore::new();
    for user in [9, 3, 9] {
      note_acknowledgement(&store, id(1), id(50), id(user)).unwrap();
    }
    assert_eq!(
      acknowledgements(&store, id(1), id(50)).unwrap(),
      vec![id(3), id(9)]
    );
    assert!(acknowledgements(&store, id(1), id(51)).unwrap().is_empty());
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
