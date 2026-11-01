//! Messages waiting for their keys (issue #888, e2ee.md §5, "History on a new device"). A message
//! this device can't read yet is parked and stands in the conversation as a placeholder
//! ([`Unreadable`]) at its place. Per conversation with waiting placeholders a small record keeps
//! when the oldest started to wait, so the upkeep finds the ones whose wait ran out without
//! reading any message; a waiting placeholder becomes "not available" then. The conversations of
//! the current catch-up are kept too, for the progress the apps show ("3 of 5 conversations caught
//! up"). Two bounds keep what the server can make this device store finite: failed placeholders
//! per conversation, and parked messages per conversation (the oldest given up on go first).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, MessageResponse};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table, Unreadable};

const KEY_WAIT_PREFIX: &str = "key_wait/";
const CATCH_UP_KEY: &str = "key_catch_up";
const FAILED_PREFIX: &str = "failed_placeholders/";

/// How far the conversations that had messages waiting for keys have caught up: every message
/// readable, or known to stay unavailable. Shown while `caught_up < total`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyCatchUp {
  pub caught_up: usize,
  pub total: usize,
  /// The conversations with messages still waiting.
  pub waiting: BTreeSet<ConversationId>,
}

/// A conversation with placeholders waiting for keys, under `key_wait/<conversation>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct KeyWait {
  /// When the conversation's oldest waiting placeholder started to wait (local clock). May be
  /// older than the oldest that still waits; the upkeep then looks and corrects it.
  oldest_since_ms: i64,
}

/// The conversations of the current catch-up: every one that had a waiting placeholder since the
/// last catch-up completed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CatchUpRound {
  conversations: BTreeSet<ConversationId>,
}

/// The catch-up's progress, or `None` when nothing waits for keys. Reads two small records.
pub fn key_catch_up(store: &impl LocalStore) -> Result<Option<KeyCatchUp>, CoreError> {
  let Some(round) = records::get_json::<CatchUpRound>(store, Table::SyncState, CATCH_UP_KEY)? else {
    return Ok(None);
  };
  let waiting = waiting_conversations(store)?;
  let total = round.conversations.len();
  let caught_up = round
    .conversations
    .iter()
    .filter(|conversation| !waiting.contains(conversation))
    .count();
  Ok((caught_up < total).then_some(KeyCatchUp {
    caught_up,
    total,
    waiting,
  }))
}

/// Notes that a placeholder of `conversation` started to wait at `now_ms`.
pub(super) fn note_waiting(
  store: &impl LocalStore,
  conversation: ConversationId,
  now_ms: i64,
) -> Result<(), CoreError> {
  let key = key_wait_key(conversation);
  let maybe_wait: Option<KeyWait> = records::get_json(store, Table::SyncState, &key)?;
  if maybe_wait.is_none_or(|wait| wait.oldest_since_ms > now_ms) {
    records::put_json(
      store,
      Table::SyncState,
      &key,
      &KeyWait {
        oldest_since_ms: maybe_wait.map_or(now_ms, |wait| wait.oldest_since_ms.min(now_ms)),
      },
    )?;
  }
  let mut round: CatchUpRound = records::get_json(store, Table::SyncState, CATCH_UP_KEY)?.unwrap_or_default();
  if round.conversations.insert(conversation) {
    records::put_json(store, Table::SyncState, CATCH_UP_KEY, &round)?;
  }
  Ok(())
}

/// Counts a new failed placeholder of `conversation` against `max`: `false` (and nothing counted)
/// once it has `max` of them.
pub(super) fn take_failed_slot(
  store: &impl LocalStore,
  conversation: ConversationId,
  max: u32,
) -> Result<bool, CoreError> {
  let key = format!("{FAILED_PREFIX}{conversation}");
  let count: u32 = records::get_json(store, Table::SyncState, &key)?.unwrap_or(0);
  if count >= max {
    return Ok(false);
  }
  records::put_json(store, Table::SyncState, &key, &(count + 1))?;
  Ok(true)
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Turns placeholders whose wait ran out (`missing_key_give_up`) into "not available" (the upkeep
  /// calls it on its local checks). Not sooner: this device can't tell that a key will never come,
  /// as long as one of the user's other devices or a member may still bring it. Reads messages only
  /// of conversations with a placeholder due, once. Ends the catch-up when nothing waits any more.
  /// Returns whether a placeholder changed.
  pub fn settle_missing_keys(&mut self) -> Result<bool, CoreError> {
    let now = self.now_ms();
    let mut is_changed = false;
    for conversation in waiting_conversations(&self.store)? {
      let maybe_wait: Option<KeyWait> =
        records::get_json(&self.store, Table::SyncState, &key_wait_key(conversation))?;
      let Some(wait) = maybe_wait else { continue };
      let wait_ms = i64::try_from(self.settings.missing_key_give_up.as_millis()).unwrap_or(i64::MAX);
      if !has_waited(wait.oldest_since_ms, now, wait_ms) {
        continue;
      }
      is_changed |= self.give_up_waiting(conversation, now, wait_ms)?;
    }
    if waiting_conversations(&self.store)?.is_empty() {
      self.store.delete(Table::SyncState, CATCH_UP_KEY)?;
    }
    Ok(is_changed)
  }

  /// Corrects the conversation's wait record after its parked messages were tried again (gone when
  /// none waits any more), and keeps at most `max_parked_per_conversation` parked: beyond it the
  /// oldest given up on are no longer kept for a retry. Their rows stay "not available".
  pub(super) fn refresh_key_wait(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let parked = self.parked(conversation)?;
    let mut maybe_oldest: Option<i64> = None;
    let mut given_up = Vec::new();
    for message in &parked {
      match self.unreadable(message)? {
        Some(Unreadable::Waiting { since_ms }) => {
          maybe_oldest = Some(maybe_oldest.map_or(since_ms, |oldest| oldest.min(since_ms)));
        },
        Some(Unreadable::NotAvailable) => given_up.push(message),
        _ => {},
      }
    }
    let excess = parked
      .len()
      .saturating_sub(self.settings.max_parked_per_conversation);
    if excess > 0 {
      given_up.sort_by_key(|message| message.id);
      for message in given_up.into_iter().take(excess) {
        self
          .store
          .delete(Table::Undecrypted, &super::messaging::undecrypted_key(message))?;
      }
      tracing::info!(conversation_id = %conversation, excess, "parked messages given up on no longer kept");
    }
    store_key_wait(&self.store, conversation, maybe_oldest)
  }

  /// Marks the conversation's placeholders that waited `wait_ms` as not available.
  fn give_up_waiting(
    &mut self,
    conversation: ConversationId,
    now: i64,
    wait_ms: i64,
  ) -> Result<bool, CoreError> {
    let mut maybe_oldest: Option<i64> = None;
    let mut is_changed = false;
    crate::store::write_group(&self.store, || -> Result<(), CoreError> {
      for message in self.parked(conversation)? {
        let Some(since_ms) = self.waiting_since(&message)? else {
          continue;
        };
        if has_waited(since_ms, now, wait_ms) {
          if let Some(mut placeholder) = self.store.message(conversation, message.client_message_id)? {
            placeholder.maybe_unreadable = Some(Unreadable::NotAvailable);
            self.store.upsert_message(&placeholder)?;
            is_changed = true;
          }
        } else {
          maybe_oldest = Some(maybe_oldest.map_or(since_ms, |oldest| oldest.min(since_ms)));
        }
      }
      store_key_wait(&self.store, conversation, maybe_oldest)
    })?;
    if is_changed {
      tracing::info!(conversation_id = %conversation, "messages without keys marked as not available");
    }
    Ok(is_changed)
  }

  fn parked(&self, conversation: ConversationId) -> Result<Vec<MessageResponse>, CoreError> {
    let parked: Vec<(String, MessageResponse)> =
      records::list_json_with_prefix(&self.store, Table::Undecrypted, &format!("{conversation}/"))?;
    Ok(parked.into_iter().map(|(_, message)| message).collect())
  }

  /// When the parked message's placeholder started to wait, if it still does.
  fn waiting_since(&self, message: &MessageResponse) -> Result<Option<i64>, CoreError> {
    Ok(match self.unreadable(message)? {
      Some(Unreadable::Waiting { since_ms }) => Some(since_ms),
      _ => None,
    })
  }

  /// The state of the parked message's placeholder, if it has one for this very message.
  fn unreadable(&self, message: &MessageResponse) -> Result<Option<Unreadable>, CoreError> {
    Ok(
      self
        .store
        .message(message.conversation_id, message.client_message_id)?
        .filter(|stored| stored.maybe_server_id == Some(message.id))
        .and_then(|stored| stored.maybe_unreadable),
    )
  }
}

fn store_key_wait(
  store: &impl LocalStore,
  conversation: ConversationId,
  maybe_oldest: Option<i64>,
) -> Result<(), CoreError> {
  let key = key_wait_key(conversation);
  match maybe_oldest {
    Some(oldest_since_ms) => records::put_json(store, Table::SyncState, &key, &KeyWait { oldest_since_ms }),
    None => Ok(store.delete(Table::SyncState, &key)?),
  }
}

/// The conversations with placeholders waiting for keys (the keys of their records).
fn waiting_conversations(store: &impl LocalStore) -> Result<BTreeSet<ConversationId>, CoreError> {
  Ok(
    store
      .list_prefix(Table::SyncState, KEY_WAIT_PREFIX)?
      .into_iter()
      .filter_map(|(key, _)| key.strip_prefix(KEY_WAIT_PREFIX)?.parse().ok())
      .collect(),
  )
}

/// Whether something that started at `since_ms` has waited `wait_ms` by `now_ms`. A start ahead
/// of now (the clock was set back) counts as long ago, so a wrong clock never makes a wait endless.
fn has_waited(since_ms: i64, now_ms: i64, wait_ms: i64) -> bool {
  since_ms > now_ms || now_ms.saturating_sub(since_ms) >= wait_ms
}

fn key_wait_key(conversation: ConversationId) -> String {
  format!("{KEY_WAIT_PREFIX}{conversation}")
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn a_wait_ends_after_its_time_and_a_start_ahead_of_now_counts_as_long_ago() {
    assert!(!has_waited(1_000, 1_009, 10));
    assert!(has_waited(1_000, 1_010, 10));
    assert!(has_waited(1_011, 1_010, 10), "the clock was set back");
  }

  #[test]
  fn progress_counts_the_conversations_that_no_longer_wait() {
    let store = MemoryStore::new();
    assert_eq!(key_catch_up(&store).unwrap(), None, "nothing waits");
    note_waiting(&store, id(1), 100).unwrap();
    note_waiting(&store, id(2), 200).unwrap();
    assert_eq!(
      key_catch_up(&store)
        .unwrap()
        .map(|progress| (progress.caught_up, progress.total)),
      Some((0, 2))
    );
    store_key_wait(&store, id(1), None).unwrap();
    assert_eq!(
      key_catch_up(&store)
        .unwrap()
        .map(|progress| (progress.caught_up, progress.total)),
      Some((1, 2))
    );
    store_key_wait(&store, id(2), None).unwrap();
    assert_eq!(key_catch_up(&store).unwrap(), None, "all caught up");
  }

  #[test]
  fn the_oldest_wait_is_kept() {
    let store = MemoryStore::new();
    note_waiting(&store, id(1), 200).unwrap();
    note_waiting(&store, id(1), 100).unwrap();
    note_waiting(&store, id(1), 300).unwrap();
    let wait: KeyWait = records::get_json(&store, Table::SyncState, &key_wait_key(id(1)))
      .unwrap()
      .unwrap();
    assert_eq!(wait.oldest_since_ms, 100);
  }

  fn id(raw: i64) -> ConversationId {
    teamcraft_proto::Id::from_snowflake(teamcraft_proto::Snowflake::new(raw).unwrap())
  }
}
