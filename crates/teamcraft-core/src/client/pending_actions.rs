//! Small actions that must reach the server even if the device is offline when the user takes them
//! (issue #159): read positions, acknowledgements, the server's pin flag, the manual presence and
//! the custom status.
//!
//! Each action is stored first and sent afterwards: right away, on every upkeep tick, and at the
//! end of a full sync, until the server took it. The queue survives a restart (it lives in the
//! store) and coalesces: one entry per target, so marking a busy channel read a hundred times while
//! offline sends one request. The entry's key is the action's idempotency key; every action is a
//! "set to" on the server (`GREATEST` for read positions, `ON CONFLICT DO NOTHING` for
//! acknowledgements, a flag for pins, a value for presence and status), so a retry after a lost response
//! changes nothing. A read position in the queue only ever moves forward.
//!
//! Every entry is its own record in [`Table::PendingActions`]: an entry this version can't read
//! (a newer version queued it, or it is damaged) costs only that entry, and two writers (web tabs
//! of the same account) never overwrite each other's entries. An entry is only replaced or removed
//! if it is still the one that was read (a compare-and-swap per entry), so a newer action queued
//! while an older one was on its way is not lost. Two tabs of different versions that write the
//! same target at once keep the last write; for read positions the server's `GREATEST` keeps that
//! harmless.
//!
//! Entries go out in the order they were queued. Each one backs off on its own when the server has
//! trouble with it, and gives up after [`PendingActionSettings::max_attempts`] or
//! [`PendingActionSettings::max_age`] (dropped with a log entry), so one stuck entry never holds
//! back the others.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ClientMessageId, ConversationId, CustomStatus, MessageId, SetCustomStatusRequest, SetPresenceRequest,
  SetReadPositionRequest, SetThreadReadRequest, ThreadId, UnixMillis,
};

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// An action waiting for the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingAction {
  /// The user read the conversation up to `message`.
  ReadPosition {
    conversation: ConversationId,
    message: MessageId,
  },
  /// The user read the thread up to the reply `message` (the thread's own read position).
  ThreadReadPosition {
    conversation: ConversationId,
    thread: ThreadId,
    message: MessageId,
  },
  /// The user confirmed an urgent message.
  Acknowledge {
    conversation: ConversationId,
    message: MessageId,
  },
  /// The server's pin flag of a message (keeps it through retention); the encrypted pin itself
  /// goes through the outbox.
  ServerPin {
    conversation: ConversationId,
    target: ClientMessageId,
    is_pinned: bool,
  },
  /// The user's manual presence (do not disturb, invisible, automatic).
  Presence { request: SetPresenceRequest },
  /// The user's custom status (PLAN.md §4).
  CustomStatus { change: StatusChange },
  /// An action a newer version queued (another tab, or before a downgrade). This version leaves
  /// the entry alone for the version that knows it.
  #[serde(other)]
  Unknown,
}

/// A custom status to set (its end on this device's clock), or none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum StatusChange {
  Set { status: CustomStatus },
  Clear,
}

/// Limits of the queue. Configurable; the defaults keep a busy device's queue small and retry a
/// server in trouble for about a day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingActionSettings {
  /// Entries kept at most; beyond it the oldest goes. Coalescing keeps real queues far below.
  pub max_entries: usize,
  /// Wait before an entry the server had trouble with (5xx, 429) is tried again; doubles with each
  /// attempt.
  pub retry_after: Duration,
  /// The wait between attempts grows up to this.
  pub retry_max: Duration,
  /// Attempts the server had trouble with before the entry is dropped.
  pub max_attempts: u32,
  /// An entry not sent within this long after it was queued is dropped.
  pub max_age: Duration,
}

/// One stored entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct QueuedAction {
  action: PendingAction,
  /// When the user last took it (this device's clock): the send order and the age cap.
  queued_at_ms: i64,
  /// Breaks ties between entries queued in the same millisecond.
  #[serde(default)]
  sequence: u64,
  /// Attempts the server had trouble with so far.
  #[serde(default)]
  attempts: u32,
  /// Not tried again before this (after server trouble).
  #[serde(default)]
  maybe_retry_at_ms: Option<i64>,
}

/// What one flush did.
#[derive(Debug, Default)]
pub(super) struct FlushOutcome {
  pub(super) sent: usize,
  /// Actions the server refused for good (dropped), with its answer.
  pub(super) refused: Vec<(PendingAction, ApiError)>,
}

/// What became of one attempt.
enum Attempt {
  Sent,
  /// Not possible yet (a pin of a message the server has no ID for); stays queued.
  Waiting,
  /// The server refused it for good (the conversation is gone, the message unknown): dropped.
  Refused(ApiError),
  /// The server had trouble with this request (5xx, 429): the entry backs off, the others go on.
  ServerTrouble(ApiError),
  /// Offline, a token to renew or an unreadable store: every other entry would fail the same way,
  /// so the flush stops and nothing counts as an attempt.
  Unreachable(CoreError),
}

impl Default for PendingActionSettings {
  fn default() -> Self {
    Self {
      max_entries: 1000,
      retry_after: Duration::from_secs(30),
      retry_max: Duration::from_secs(60 * 60),
      max_attempts: 30,
      max_age: Duration::from_secs(7 * 24 * 60 * 60),
    }
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Queues the action and tries to send what is queued. Never fails for being offline: the action
  /// is stored and goes out later.
  pub async fn queue_action(&mut self, action: PendingAction) -> Result<(), CoreError> {
    if action == PendingAction::Unknown {
      tracing::warn!("an unknown action can't be queued");
      return Ok(());
    }
    self.store_action(action)?;
    self.flush_pending_actions().await?;
    Ok(())
  }

  /// Sends the queued actions in the order they were queued. Skips those that wait for their
  /// retry time, drops those the server refuses for good or that ran out of attempts or age, and
  /// stops when the server can't be reached. Returns how many were sent. A refused custom status
  /// waits for the host to show it ([`Client::take_background_failures`]): nobody waits for this
  /// flush (issue #322).
  pub async fn flush_pending_actions(&mut self) -> Result<usize, CoreError> {
    let outcome = self.flush_queue().await?;
    for (action, error) in outcome.refused {
      if let PendingAction::CustomStatus { .. } = action {
        self.background_failures.push(CoreError::Api(error));
      }
    }
    Ok(outcome.sent)
  }

  /// How many actions wait for the server (those of a newer version not counted).
  pub fn pending_action_count(&self) -> Result<usize, CoreError> {
    Ok(
      load_entries(&self.store)?
        .iter()
        .filter(|(_, queued)| queued.action != PendingAction::Unknown)
        .count(),
    )
  }

  /// The custom status change still waiting for the server, if one is.
  pub(super) fn queued_status_change(&self) -> Result<Option<StatusChange>, CoreError> {
    let key = PendingAction::CustomStatus {
      change: StatusChange::Clear,
    }
    .key();
    Ok(
      load_entry(&self.store, &key)?.and_then(|queued| match queued.action {
        PendingAction::CustomStatus { change } => Some(change),
        _ => None,
      }),
    )
  }

  /// [`Self::flush_pending_actions`], also telling which actions the server refused.
  pub(super) async fn flush_queue(&mut self) -> Result<FlushOutcome, CoreError> {
    let entries = load_entries(&self.store)?;
    let now_ms = self.now_ms();
    let mut outcome = FlushOutcome::default();
    for (key, queued) in entries {
      if queued.action == PendingAction::Unknown {
        continue;
      }
      if self.is_too_old(&queued, now_ms) {
        tracing::warn!(
          kind = queued.action.kind(),
          "a queued action got too old; dropped"
        );
        replace_if_unchanged(&self.store, &key, &queued, None)?;
        continue;
      }
      if queued
        .maybe_retry_at_ms
        .is_some_and(|retry_at_ms| retry_at_ms > now_ms)
      {
        continue;
      }
      match self.attempt(&queued.action).await {
        Attempt::Sent => {
          // A newer action for the same target queued meanwhile stays and goes out next time.
          replace_if_unchanged(&self.store, &key, &queued, None)?;
          outcome.sent += 1;
          self.after_sent(&queued.action).await;
        },
        Attempt::Waiting => {},
        Attempt::Refused(error) => {
          tracing::warn!(%error, kind = queued.action.kind(), "the server refused a queued action; dropped");
          replace_if_unchanged(&self.store, &key, &queued, None)?;
          self.after_refused(&queued.action).await;
          outcome.refused.push((queued.action, error));
        },
        Attempt::ServerTrouble(error) => self.back_off(&key, &queued, &error, now_ms)?,
        Attempt::Unreachable(error) => {
          tracing::debug!(%error, "queued actions wait for the server");
          break;
        },
      }
    }
    Ok(outcome)
  }

  pub(super) fn store_action(&self, action: PendingAction) -> Result<(), CoreError> {
    let key = action.key();
    let maybe_queued = load_entry(&self.store, &key)?;
    let action = match (maybe_queued.as_ref().map(|queued| &queued.action), action) {
      // A read position never moves back, also not when an older one is queued after a newer one.
      (
        Some(PendingAction::ReadPosition { message: queued, .. }),
        PendingAction::ReadPosition {
          conversation,
          message,
        },
      ) => PendingAction::ReadPosition {
        conversation,
        message: message.max(*queued),
      },
      (
        Some(PendingAction::ThreadReadPosition { message: queued, .. }),
        PendingAction::ThreadReadPosition {
          conversation,
          thread,
          message,
        },
      ) => PendingAction::ThreadReadPosition {
        conversation,
        thread,
        message: message.max(*queued),
      },
      (_, action) => action,
    };
    let entries = load_entries(&self.store)?;
    let sequence = entries
      .iter()
      .map(|(_, queued)| queued.sequence)
      .max()
      .map_or(0, |sequence| sequence.saturating_add(1));
    let queued = QueuedAction {
      action,
      queued_at_ms: self.now_ms(),
      sequence,
      attempts: 0,
      maybe_retry_at_ms: None,
    };
    records::put_json(&self.store, Table::PendingActions, &key, &queued)?;
    let others: Vec<(String, QueuedAction)> =
      entries.into_iter().filter(|(other, _)| *other != key).collect();
    self.trim_queue(others.len() + 1, &others)
  }

  /// Drops the oldest entries beyond [`PendingActionSettings::max_entries`]; `entries` are the
  /// others, oldest first.
  fn trim_queue(&self, count: usize, entries: &[(String, QueuedAction)]) -> Result<(), CoreError> {
    let excess = count.saturating_sub(self.settings.pending_actions.max_entries);
    for (key, _) in entries.iter().take(excess) {
      tracing::warn!("too many queued actions; the oldest is dropped");
      self.store.delete(Table::PendingActions, key)?;
    }
    Ok(())
  }

  fn is_too_old(&self, queued: &QueuedAction, now_ms: i64) -> bool {
    let max_age_ms = duration_ms(self.settings.pending_actions.max_age);
    now_ms.saturating_sub(queued.queued_at_ms) > max_age_ms
  }

  /// After server trouble: the entry waits twice as long as last time, or goes once it ran out of
  /// attempts.
  fn back_off(
    &self,
    key: &str,
    queued: &QueuedAction,
    error: &ApiError,
    now_ms: i64,
  ) -> Result<(), CoreError> {
    let limits = &self.settings.pending_actions;
    let attempts = queued.attempts.saturating_add(1);
    if attempts >= limits.max_attempts {
      tracing::warn!(%error, kind = queued.action.kind(), attempts, "a queued action ran out of attempts; dropped");
      return replace_if_unchanged(&self.store, key, queued, None).map(|_| ());
    }
    tracing::debug!(%error, kind = queued.action.kind(), attempts, "a queued action waits for the server");
    let doubled = limits
      .retry_after
      .saturating_mul(2_u32.saturating_pow(attempts.saturating_sub(1)));
    let wait_ms = duration_ms(doubled.min(limits.retry_max));
    let retried = QueuedAction {
      attempts,
      maybe_retry_at_ms: Some(now_ms.saturating_add(wait_ms)),
      ..queued.clone()
    };
    replace_if_unchanged(&self.store, key, queued, Some(&retried)).map(|_| ())
  }

  async fn attempt(&mut self, action: &PendingAction) -> Attempt {
    let result = match action {
      PendingAction::ReadPosition {
        conversation,
        message,
      } => {
        let request = SetReadPositionRequest { message_id: *message };
        self.api.set_read_position(*conversation, &request).await
      },
      PendingAction::ThreadReadPosition {
        conversation,
        thread,
        message,
      } => {
        let request = SetThreadReadRequest { message_id: *message };
        self.api.set_thread_read(*conversation, *thread, &request).await
      },
      PendingAction::Acknowledge {
        conversation,
        message,
      } => self.api.acknowledge_message(*conversation, *message).await,
      PendingAction::ServerPin {
        conversation,
        target,
        is_pinned,
      } => match self.store.message(*conversation, *target) {
        Ok(Some(stored)) => match stored.maybe_server_id {
          Some(message) => {
            self
              .api
              .set_message_pinned(*conversation, message, *is_pinned)
              .await
          },
          // Still in the outbox: the flag follows once the message has its server ID.
          None => return Attempt::Waiting,
        },
        // The message is gone from this device (deleted, conversation left): nothing to flag.
        Ok(None) => return Attempt::Sent,
        Err(error) => return Attempt::Unreachable(error.into()),
      },
      PendingAction::Presence { request } => self.api.set_presence(request).await,
      PendingAction::CustomStatus { change } => {
        let maybe_request = match change {
          StatusChange::Set { status } => {
            SetCustomStatusRequest::for_status(status, UnixMillis(self.now_ms()))
          },
          StatusChange::Clear => None,
        };
        match maybe_request {
          Some(request) => self.api.set_custom_status(&request).await,
          // A clear, or a status that ended while it waited (offline): nothing left to show.
          None => self.api.clear_custom_status().await,
        }
      },
      // Never attempted (the flush skips them); nothing this version could send.
      PendingAction::Unknown => return Attempt::Waiting,
    };
    match result {
      Ok(()) => Attempt::Sent,
      Err(error @ ApiError::Status { .. }) if error.is_transient() => Attempt::ServerTrouble(error),
      Err(error) if error.is_retry_later() => Attempt::Unreachable(CoreError::Api(error)),
      Err(error) => Attempt::Refused(error),
    }
  }

  /// What follows a refused action: the custom status shown at once goes back to the server's.
  /// Failures here are not the action's: logged only.
  async fn after_refused(&mut self, action: &PendingAction) {
    if let PendingAction::CustomStatus { .. } = action {
      let me = self.account.user_id;
      if let Err(error) = self.refresh_presence(&[me]).await {
        tracing::debug!(%error, "own status not read again after the server refused it");
      }
    }
  }

  /// What follows a sent action. Failures here are not the action's: logged only.
  async fn after_sent(&mut self, action: &PendingAction) {
    if let PendingAction::ReadPosition { conversation, .. } = action
      && let Err(error) = self.refresh_conversation(*conversation).await
    {
      // The `unread_update` event brings the server's counts as well.
      tracing::debug!(%error, "conversation not refreshed after its read position");
    }
  }
}

impl PendingAction {
  /// The target the action is about: a newer action for the same target replaces the older one.
  fn key(&self) -> String {
    match self {
      Self::ReadPosition { conversation, .. } => format!("read/{conversation}"),
      Self::ThreadReadPosition {
        conversation, thread, ..
      } => format!("thread_read/{conversation}/{thread}"),
      Self::Acknowledge {
        conversation,
        message,
      } => format!("ack/{conversation}/{message}"),
      Self::ServerPin {
        conversation, target, ..
      } => format!("pin/{conversation}/{target}"),
      Self::Presence { .. } => "presence".to_owned(),
      Self::CustomStatus { .. } => "custom_status".to_owned(),
      Self::Unknown => "unknown".to_owned(),
    }
  }

  /// For logs: never the content, only what kind of action it was.
  fn kind(&self) -> &'static str {
    match self {
      Self::ReadPosition { .. } => "read_position",
      Self::ThreadReadPosition { .. } => "thread_read_position",
      Self::Acknowledge { .. } => "acknowledge",
      Self::ServerPin { .. } => "server_pin",
      Self::Presence { .. } => "presence",
      Self::CustomStatus { .. } => "custom_status",
      Self::Unknown => "unknown",
    }
  }
}

impl QueuedAction {
  fn order(&self) -> (i64, u64) {
    (self.queued_at_ms, self.sequence)
  }
}

/// Every readable entry, oldest first. A damaged entry is dropped with a log entry; it would never
/// become readable.
fn load_entries(store: &impl LocalStore) -> Result<Vec<(String, QueuedAction)>, CoreError> {
  let mut entries = Vec::new();
  for (key, bytes) in store.list(Table::PendingActions)? {
    match serde_json::from_slice::<QueuedAction>(&bytes) {
      Ok(queued) => entries.push((key, queued)),
      Err(error) => {
        tracing::warn!(%error, "a damaged queued action is dropped");
        store.delete(Table::PendingActions, &key)?;
      },
    }
  }
  entries.sort_by(|(left_key, left), (right_key, right)| {
    left
      .order()
      .cmp(&right.order())
      .then_with(|| left_key.cmp(right_key))
  });
  Ok(entries)
}

/// The entry for `key`; a damaged one counts as absent (it is overwritten).
fn load_entry(store: &impl LocalStore, key: &str) -> Result<Option<QueuedAction>, CoreError> {
  match records::get_json(store, Table::PendingActions, key) {
    Ok(maybe_queued) => Ok(maybe_queued),
    Err(CoreError::Corrupt(_)) => Ok(None),
    Err(error) => Err(error),
  }
}

/// Replaces (or with `None` removes) the entry if it is still `expected`: an entry another writer
/// changed meanwhile is the newer one and stays. Returns whether it was replaced.
fn replace_if_unchanged(
  store: &impl LocalStore,
  key: &str,
  expected: &QueuedAction,
  maybe_new: Option<&QueuedAction>,
) -> Result<bool, CoreError> {
  if load_entry(store, key)?.as_ref() != Some(expected) {
    return Ok(false);
  }
  match maybe_new {
    Some(new) => records::put_json(store, Table::PendingActions, key, new)?,
    None => store.delete(Table::PendingActions, key)?,
  }
  Ok(true)
}

fn duration_ms(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn an_entry_of_an_unknown_kind_is_read_as_unknown_and_left_alone() {
    let store = MemoryStore::new();
    let newer = br#"{"action":{"kind":"set_status","text":"away"},"queued_at_ms":5}"#;
    store.put(Table::PendingActions, "status", newer).unwrap();
    let entries = load_entries(&store).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1.action, PendingAction::Unknown);
    assert!(store.get(Table::PendingActions, "status").unwrap().is_some());
  }

  #[test]
  fn a_damaged_entry_is_dropped_and_the_others_still_load() {
    let store = MemoryStore::new();
    store.put(Table::PendingActions, "broken", b"{not json").unwrap();
    let queued = queued(read(1, 10), 1);
    records::put_json(&store, Table::PendingActions, "read/1", &queued).unwrap();
    let entries = load_entries(&store).unwrap();
    assert_eq!(entries, vec![("read/1".to_owned(), queued)]);
    assert!(store.get(Table::PendingActions, "broken").unwrap().is_none());
  }

  #[test]
  fn entries_load_in_the_order_they_were_queued() {
    let store = MemoryStore::new();
    records::put_json(&store, Table::PendingActions, "b", &queued(read(1, 10), 1)).unwrap();
    records::put_json(&store, Table::PendingActions, "a", &queued(read(2, 10), 2)).unwrap();
    let keys: Vec<String> = load_entries(&store)
      .unwrap()
      .into_iter()
      .map(|(key, _)| key)
      .collect();
    assert_eq!(keys, vec!["b", "a"]);
  }

  #[test]
  fn an_entry_changed_by_another_writer_is_not_replaced() {
    let store = MemoryStore::new();
    let seen = queued(read(1, 10), 1);
    let newer = queued(read(1, 20), 2);
    records::put_json(&store, Table::PendingActions, "read/1", &newer).unwrap();
    assert!(!replace_if_unchanged(&store, "read/1", &seen, None).unwrap());
    assert_eq!(load_entry(&store, "read/1").unwrap(), Some(newer.clone()));
    assert!(replace_if_unchanged(&store, "read/1", &newer, None).unwrap());
    assert_eq!(load_entry(&store, "read/1").unwrap(), None);
  }

  fn queued(action: PendingAction, queued_at_ms: i64) -> QueuedAction {
    QueuedAction {
      action,
      queued_at_ms,
      sequence: 0,
      attempts: 0,
      maybe_retry_at_ms: None,
    }
  }

  fn read(conversation: i64, message: i64) -> PendingAction {
    PendingAction::ReadPosition {
      conversation: ConversationId::from_snowflake(Snowflake::new(conversation).unwrap()),
      message: MessageId::from_snowflake(Snowflake::new(message).unwrap()),
    }
  }
}
