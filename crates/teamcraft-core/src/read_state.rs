//! Read state the server reports: followed threads, unread counts, other members' read receipts
//! and acknowledgements of urgent messages. The sync engine writes it from gateway events; UIs read
//! it with the functions here.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::live::{AnnouncedPresence, LiveMarks, LiveReceipt, RevisedPresence};
use teamcraft_proto::{
  ConversationId, ConversationUnread, MessageId, MessageResponse, ThreadId, ThreadSummary, UnreadResponse,
  UserId, UserPresence,
};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

const UNREAD_KEY: &str = "state";
const MARKS_KEY: &str = "marks";
/// The record in [`Table::Presence`] (whose other keys are user IDs) that heads the presence log
/// in [`Table::AnnouncedPresence`]. Before 1.0.2 it held the whole log; that form reads as a head
/// with an empty log.
pub(crate) const ANNOUNCED_PRESENCE_KEY: &str = "announced";
/// What servers before 1.0.1 cap counts at (`limits.max_unread_counted` default); newer servers
/// say it in `GET /unread`.
const DEFAULT_MAX_UNREAD_COUNTED: u32 = 1000;

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
pub fn all_read_receipts(store: &impl LocalStore) -> Result<Vec<LiveReceipt>, CoreError> {
  let mut receipts = Vec::new();
  for (key, value) in store.list(Table::ReadReceipts)? {
    let corrupt = || CoreError::Corrupt(format!("read receipt key {key}"));
    let (conversation, user) = key.split_once('/').ok_or_else(corrupt)?;
    receipts.push(LiveReceipt {
      conversation: conversation.parse().map_err(|_| corrupt())?,
      user: user.parse().map_err(|_| corrupt())?,
      message: serde_json::from_slice(&value).map_err(|error| CoreError::Corrupt(error.to_string()))?,
    });
  }
  Ok(receipts)
}

/// Presence and custom status as `presence_update` events announced them, bounded.
pub fn announced_presence(store: &impl LocalStore) -> Result<AnnouncedPresence, CoreError> {
  announced_presence_since(store, 0)
}

/// The announcements newer than `revision`, oldest first (issue #259: the UI gets what changed,
/// not the whole log). Costs one record read when nothing is newer.
pub fn announced_presence_since(
  store: &impl LocalStore,
  revision: u64,
) -> Result<AnnouncedPresence, CoreError> {
  let head = presence_log(store)?;
  let mut entries = Vec::new();
  if head.revision > revision {
    entries = presence_log_entries(store)?;
    entries.retain(|entry| entry.revision > revision);
  }
  Ok(AnnouncedPresence {
    entries,
    revision: head.revision,
    evicted_up_to: head.evicted_up_to,
  })
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

/// Counts a new top-level message of someone else, as the server does (threads.rs counts replies),
/// up to the server's cap. Only for messages that arrive live: history that sync loads was counted
/// by the server already, and so was a message this device stored before (a duplicate delivery) or
/// one the last `GET /unread` covered (`counted_through`).
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
  // Issue #259: events that queued while `GET /unread` was answered are in its counts already.
  if unread
    .maybe_counted_through
    .is_some_and(|counted_through| message.id <= counted_through)
  {
    return Ok(());
  }
  let is_known = store
    .message(message.conversation_id, message.client_message_id)?
    .is_some_and(|stored| stored.maybe_server_id.is_some());
  if is_known {
    return Ok(());
  }
  let max_counted = unread.maybe_max_counted.unwrap_or(DEFAULT_MAX_UNREAD_COUNTED);
  let mention = u32::from(message.mentioned_user_ids.contains(&me));
  match unread
    .conversations
    .iter_mut()
    .find(|entry| entry.conversation_id == message.conversation_id)
  {
    Some(entry) => {
      // Mentions are counted among the counted messages (proto `ConversationUnread`).
      if entry.unread_count >= max_counted {
        return Ok(());
      }
      entry.unread_count += 1;
      // The server's counts can be anything: an overflow would panic in debug builds.
      entry.mention_count = entry
        .mention_count
        .saturating_add(mention)
        .min(entry.unread_count);
    },
    None if max_counted == 0 => return Ok(()),
    None => unread.conversations.push(ConversationUnread {
      conversation_id: message.conversation_id,
      unread_count: 1,
      mention_count: mention,
    }),
  }
  save_unread(store, &unread)
}

/// Keeps a person's presence for the UI under the next revision: one record per person, so an
/// announcement writes that person's record and the log's head, not the whole log (issue #259).
/// The same presence again is no announcement. Beyond `max_entries` people the oldest tenth is
/// dropped at once, so the bound costs one pass over the log now and then, not one per event.
/// Returns whether it was announced.
pub(crate) fn note_announced_presence(
  store: &impl LocalStore,
  presence: UserPresence,
  max_entries: usize,
) -> Result<bool, CoreError> {
  let key = presence.user_id.to_string();
  let maybe_known: Option<RevisedPresence> = records::get_json(store, Table::AnnouncedPresence, &key)?;
  if maybe_known
    .as_ref()
    .is_some_and(|known| known.presence == presence)
  {
    return Ok(false);
  }
  let mut head = presence_log(store)?;
  if maybe_known.is_none() {
    head.len += 1;
  }
  head.revision += 1;
  let entry = RevisedPresence {
    revision: head.revision,
    presence,
  };
  records::put_json(store, Table::AnnouncedPresence, &key, &entry)?;
  if head.len > max_entries {
    evict_oldest(store, &mut head, max_entries)?;
  }
  records::put_json(store, Table::Presence, ANNOUNCED_PRESENCE_KEY, &head)?;
  Ok(true)
}

/// Drops the oldest announcements down to nine tenths of the bound.
fn evict_oldest(
  store: &impl LocalStore,
  head: &mut PresenceLog,
  max_entries: usize,
) -> Result<(), CoreError> {
  let entries = presence_log_entries(store)?;
  let keep = max_entries.saturating_sub(max_entries / 10);
  let excess = entries.len().saturating_sub(keep);
  for dropped in &entries[..excess] {
    store.delete(Table::AnnouncedPresence, &dropped.presence.user_id.to_string())?;
    head.evicted_up_to = head.evicted_up_to.max(dropped.revision);
  }
  head.len = entries.len() - excess;
  Ok(())
}

/// The head of the presence log: what [`AnnouncedPresence`] says besides its entries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PresenceLog {
  #[serde(default)]
  revision: u64,
  #[serde(default)]
  evicted_up_to: u64,
  /// How many people the log holds.
  #[serde(default)]
  len: usize,
}

fn presence_log(store: &impl LocalStore) -> Result<PresenceLog, CoreError> {
  Ok(records::get_json(store, Table::Presence, ANNOUNCED_PRESENCE_KEY)?.unwrap_or_default())
}

/// Every entry of the log, oldest first.
fn presence_log_entries(store: &impl LocalStore) -> Result<Vec<RevisedPresence>, CoreError> {
  let mut entries: Vec<RevisedPresence> = records::list_json(store, Table::AnnouncedPresence)?;
  entries.sort_by_key(|entry| entry.revision);
  Ok(entries)
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

  mod presence_log {
    use teamcraft_proto::PresenceState;

    use super::*;

    #[test]
    fn readers_get_only_newer_announcements() {
      let store = MemoryStore::new();
      for user in 1..=3 {
        note_announced_presence(&store, presence(user, PresenceState::Online), 10).unwrap();
      }
      note_announced_presence(&store, presence(1, PresenceState::Away), 10).unwrap();
      let newer = announced_presence_since(&store, 2).unwrap();
      let seen: Vec<(u64, PresenceState)> = newer
        .entries
        .iter()
        .map(|entry| (entry.revision, entry.presence.state.clone()))
        .collect();
      assert_eq!(seen, vec![(3, PresenceState::Online), (4, PresenceState::Away)]);
      assert_eq!(newer.revision, 4);
      assert!(announced_presence_since(&store, 4).unwrap().entries.is_empty());
      assert_eq!(announced_presence(&store).unwrap().entries.len(), 3);
    }

    #[test]
    fn the_same_presence_again_is_no_announcement() {
      let store = MemoryStore::new();
      assert!(note_announced_presence(&store, presence(1, PresenceState::Online), 10).unwrap());
      assert!(!note_announced_presence(&store, presence(1, PresenceState::Online), 10).unwrap());
      assert_eq!(announced_presence(&store).unwrap().revision, 1);
    }

    #[test]
    fn the_bound_drops_the_oldest_tenth_and_reports_the_gap() {
      let store = MemoryStore::new();
      for user in 1..=11 {
        note_announced_presence(&store, presence(user, PresenceState::Online), 10).unwrap();
      }
      let log = announced_presence(&store).unwrap();
      // Eleven people, bound ten: down to nine, the two oldest are gone.
      assert_eq!(log.entries.len(), 9);
      assert_eq!(log.evicted_up_to, 2);
      assert!(log.has_gap_since(1));
      // The next one fits again without dropping anybody.
      note_announced_presence(&store, presence(12, PresenceState::Online), 10).unwrap();
      assert_eq!(announced_presence(&store).unwrap().entries.len(), 10);
    }

    #[test]
    fn the_log_of_older_versions_reads_as_an_empty_log() {
      let store = MemoryStore::new();
      let mut old = AnnouncedPresence::default();
      old.note(presence(1, PresenceState::Online), 10);
      records::put_json(&store, Table::Presence, ANNOUNCED_PRESENCE_KEY, &old).unwrap();
      let log = announced_presence(&store).unwrap();
      assert!(log.entries.is_empty());
      assert_eq!(log.revision, 1);
      note_announced_presence(&store, presence(2, PresenceState::Away), 10).unwrap();
      assert_eq!(announced_presence_since(&store, 1).unwrap().entries.len(), 1);
    }

    fn presence(user: i64, state: PresenceState) -> UserPresence {
      UserPresence {
        user_id: id(user),
        state,
        maybe_status: None,
        maybe_last_seen: None,
      }
    }
  }

  mod new_messages {
    use super::*;

    #[test]
    fn counting_stops_at_the_servers_cap() {
      let store = MemoryStore::new();
      save_unread(&store, &unread_of(&[(1, 2, 2)], Some(3))).unwrap();
      for seq in 0..3 {
        note_new_message(&store, &message(1, 9, seq, true), id(5)).unwrap();
      }
      assert_eq!(
        counts(&store, 1),
        (3, 3),
        "mentions are among the counted messages"
      );
    }

    #[test]
    fn servers_without_a_cap_get_the_default_one() {
      let store = MemoryStore::new();
      save_unread(&store, &unread_of(&[(1, DEFAULT_MAX_UNREAD_COUNTED, 0)], None)).unwrap();
      note_new_message(&store, &message(1, 9, 0, false), id(5)).unwrap();
      assert_eq!(counts(&store, 1), (DEFAULT_MAX_UNREAD_COUNTED, 0));
    }

    #[test]
    fn own_messages_and_known_messages_are_not_counted() {
      let store = MemoryStore::new();
      save_unread(&store, &unread_of(&[], Some(1000))).unwrap();
      note_new_message(&store, &message(1, 5, 0, false), id(5)).unwrap();
      assert_eq!(counts(&store, 1), (0, 0), "own message");
      let delivered = message(1, 9, 1, false);
      note_new_message(&store, &delivered, id(5)).unwrap();
      store
        .upsert_message(&crate::store::StoredMessage {
          conversation_id: delivered.conversation_id,
          client_message_id: delivered.client_message_id,
          maybe_server_id: Some(delivered.id),
          sender_user_id: delivered.sender_user_id,
          maybe_thread_id: None,
          sent_at_ms: 0,
          content: crate::store::MessageContent::text("hi", Vec::new()),
          is_edited: false,
          edits: Vec::new(),
          is_pinned: false,
        })
        .unwrap();
      note_new_message(&store, &delivered, id(5)).unwrap();
      assert_eq!(counts(&store, 1), (1, 0), "a duplicate delivery counts once");
    }

    #[test]
    fn messages_the_last_answer_counted_are_not_counted_again() {
      let store = MemoryStore::new();
      let mut unread = unread_of(&[(1, 2, 0)], Some(1000));
      // Messages 100 and 101 were in the answer; their events were queued while it was on its way.
      unread.maybe_counted_through = Some(id(101));
      save_unread(&store, &unread).unwrap();
      for seq in 0..3 {
        note_new_message(&store, &message(1, 9, seq, false), id(5)).unwrap();
      }
      assert_eq!(counts(&store, 1), (3, 0), "only message 102 is new");
    }

    #[test]
    fn a_huge_mention_count_from_the_server_does_not_overflow() {
      let store = MemoryStore::new();
      save_unread(&store, &unread_of(&[(1, 5, u32::MAX)], Some(1000))).unwrap();
      note_new_message(&store, &message(1, 9, 0, true), id(5)).unwrap();
      assert_eq!(counts(&store, 1), (6, 6));
    }

    fn message(conversation: i64, sender: i64, seq: u8, is_mention: bool) -> MessageResponse {
      MessageResponse {
        id: id(100 + i64::from(seq)),
        client_message_id: teamcraft_proto::ClientMessageId::from_bytes([seq; 16]),
        conversation_id: id(conversation),
        sender_user_id: id(sender),
        sender_device_id: id(1),
        maybe_thread_id: None,
        mentioned_user_ids: if is_mention { vec![id(5)] } else { Vec::new() },
        is_urgent: false,
        content_key_id: teamcraft_proto::Base64Bytes::new(Vec::new()),
        ciphertext: teamcraft_proto::Base64Bytes::new(Vec::new()),
      }
    }

    fn unread_of(counts: &[(i64, u32, u32)], maybe_max_counted: Option<u32>) -> UnreadResponse {
      UnreadResponse {
        conversations: counts
          .iter()
          .map(|(conversation, unread_count, mention_count)| ConversationUnread {
            conversation_id: id(*conversation),
            unread_count: *unread_count,
            mention_count: *mention_count,
          })
          .collect(),
        unread_thread_count: 0,
        thread_mention_count: 0,
        maybe_max_counted,
        maybe_counted_through: None,
      }
    }

    fn counts(store: &MemoryStore, conversation: i64) -> (u32, u32) {
      unread(store)
        .unwrap()
        .unwrap()
        .conversations
        .iter()
        .find(|entry| entry.conversation_id == id(conversation))
        .map_or((0, 0), |entry| (entry.unread_count, entry.mention_count))
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
