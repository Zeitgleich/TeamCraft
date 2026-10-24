//! Read state the server reports: followed threads, unread counts, other members' read receipts
//! and acknowledgements of urgent messages. The sync engine writes it from gateway events; UIs read
//! it with the functions here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use teamcraft_proto::{
  ConversationId, ConversationUnread, MessageId, MessageResponse, ThreadId, ThreadSummary, UnreadResponse,
  UserId, UserPresence,
};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

const UNREAD_KEY: &str = "state";
const MARKS_KEY: &str = "marks";
/// Prefix of the presence records `presence_update` events leave in [`Table::Presence`].
const ANNOUNCED_PRESENCE_PREFIX: &str = "announced:";

/// Counters that move when a UI should read something from the server once: after each gateway
/// connection (events in between may be lost) and when the announcement changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveMarks {
  pub connections: u64,
  pub announcements: u64,
}

/// How far another member of a DM has read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredReceipt {
  pub conversation: ConversationId,
  pub user: UserId,
  pub message: MessageId,
}

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

/// How far other members of DMs have read, across all conversations.
pub fn all_read_receipts(store: &impl LocalStore) -> Result<Vec<StoredReceipt>, CoreError> {
  let mut receipts = Vec::new();
  for (key, value) in store.list(Table::ReadReceipts)? {
    let corrupt = || CoreError::Corrupt(format!("read receipt key {key}"));
    let (conversation, user) = key.split_once('/').ok_or_else(corrupt)?;
    receipts.push(StoredReceipt {
      conversation: conversation.parse().map_err(|_| corrupt())?,
      user: user.parse().map_err(|_| corrupt())?,
      message: serde_json::from_slice(&value).map_err(|error| CoreError::Corrupt(error.to_string()))?,
    });
  }
  Ok(receipts)
}

/// Presence and custom status as `presence_update` events announced them.
pub fn announced_presence(store: &impl LocalStore) -> Result<Vec<UserPresence>, CoreError> {
  let mut presence = Vec::new();
  for (key, value) in store.list(Table::Presence)? {
    if key.starts_with(ANNOUNCED_PRESENCE_PREFIX) {
      presence.push(serde_json::from_slice(&value).map_err(|error| CoreError::Corrupt(error.to_string()))?);
    }
  }
  Ok(presence)
}

pub fn marks(store: &impl LocalStore) -> Result<LiveMarks, CoreError> {
  Ok(records::get_json(store, Table::Unread, MARKS_KEY)?.unwrap_or_default())
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

/// Takes the server's counts of one conversation (`unread_update`). Before the first full answer
/// there is nothing to amend; the next sync brings everything.
pub(crate) fn note_unread(
  store: &impl LocalStore,
  conversation: ConversationId,
  unread_count: u32,
  mention_count: u32,
) -> Result<(), CoreError> {
  let Some(mut unread) = unread(store)? else {
    return Ok(());
  };
  unread
    .conversations
    .retain(|entry| entry.conversation_id != conversation);
  if unread_count > 0 || mention_count > 0 {
    unread.conversations.push(ConversationUnread {
      conversation_id: conversation,
      unread_count,
      mention_count,
    });
  }
  save_unread(store, &unread)
}

/// Counts a new top-level message of someone else, as the server does (threads.rs counts replies).
/// Only for messages that arrive live: history that sync loads was counted by the server already.
pub(crate) fn note_new_message(
  store: &impl LocalStore,
  message: &MessageResponse,
  me: UserId,
) -> Result<(), CoreError> {
  if message.sender_user_id == me || message.maybe_thread_id.is_some() {
    return Ok(());
  }
  let Some(mut unread) = unread(store)? else {
    return Ok(());
  };
  let is_mention = message.mentioned_user_ids.contains(&me);
  match unread
    .conversations
    .iter_mut()
    .find(|entry| entry.conversation_id == message.conversation_id)
  {
    Some(entry) => {
      entry.unread_count = entry.unread_count.saturating_add(1);
      entry.mention_count = entry.mention_count.saturating_add(u32::from(is_mention));
    },
    None => unread.conversations.push(ConversationUnread {
      conversation_id: message.conversation_id,
      unread_count: 1,
      mention_count: u32::from(is_mention),
    }),
  }
  save_unread(store, &unread)
}

pub(crate) fn note_announced_presence(
  store: &impl LocalStore,
  presence: &UserPresence,
) -> Result<(), CoreError> {
  records::put_json(
    store,
    Table::Presence,
    &format!("{ANNOUNCED_PRESENCE_PREFIX}{}", presence.user_id),
    presence,
  )
}

pub(crate) fn update_marks(
  store: &impl LocalStore,
  change: impl FnOnce(&mut LiveMarks),
) -> Result<(), CoreError> {
  let mut current = marks(store)?;
  change(&mut current);
  records::put_json(store, Table::Unread, MARKS_KEY, &current)
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
