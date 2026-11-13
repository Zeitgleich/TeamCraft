//! A device that is a member on the server but can't write asks the members again (e2ee.md §4.6,
//! "Asking again for a lost hand-over", issue #1878). Every way into a group ends with one member's
//! device doing one thing (committing an addition it holds, adding a further device, handing over
//! the current key); when that is lost, the device would otherwise wait for good with its messages
//! held back.
//!
//! - In the group without a current key, it posts a current-key request: members hand the current
//!   key to the group again, ranked as for a further device's join.
//! - Outside the group, with a message held back, it posts the device-add request: members add a
//!   further device as before, and a member holding the user's pending addition makes it due.
//!
//! A request adds nobody on its own and carries no signature: it only makes due what a member
//! already decided or would do for a join. The device asks after `key_reask_retry`, then twice as
//! long each time up to `key_reask_retry_max`, and on a new gateway connection (never twice within
//! `key_reask_retry`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, ConversationId, DeviceId, MlsMessageKind, PostMlsMessageRequest};

use super::Client;
use super::member_device::is_within;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::retry::{RetryPolicy, millis};
use crate::store::{LocalStore, Table};

/// Marks an MLS "proposal" entry in which a device of the group asks for the current content key.
pub(crate) const CURRENT_KEY_REQUEST_MAGIC: &[u8] = b"TCKR";

/// Where this device notes, per conversation, that it can't write there and when it asked.
const KEY_REASK_PREFIX: &str = "key_reask/";
/// Where the outbox notes messages held back for a missing key, and a joined device its wait for
/// the current key (`member_device`).
const HELD_FOR_KEY_PREFIX: &str = "held_for_key/";
const CURRENT_KEY_WAIT_PREFIX: &str = "current_key_wait/";

/// What made this round of asking again run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaskTrigger {
  /// The periodic check: asks where the backoff ran out.
  Timer,
  /// A new gateway connection (reconnect, resume, app start): asks at once where the device
  /// waited before, unless it asked within `key_reask_retry`.
  NewConnection,
}

/// What this device lacks to write in a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyNeed {
  /// In the group, without a current content key.
  CurrentKey,
  /// Not in the group, with a message held back for it.
  Group,
}

/// That this device can't write in a conversation: since when, and its requests so far.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct KeyReaskRecord {
  conversation: ConversationId,
  /// When this device first saw that it can't write there.
  since_ms: i64,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  maybe_asked_at_ms: Option<i64>,
  /// Requests posted so far (also failed posts); the wait before the next one doubles with each.
  attempts: u32,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Asks the members again where this device can't write and the wait ran out (or, on
  /// `NewConnection`, where it waited before). Local reads while nothing waits. A post that fails
  /// counts as asked and waits for the next turn; other conversations still ask. Returns how many
  /// requests went out.
  pub async fn ask_again_for_keys(&mut self, trigger: ReaskTrigger) -> Result<usize, CoreError> {
    let now = self.now_ms();
    let mut posted = 0;
    let mut maybe_first_error = None;
    for conversation in self.reask_candidates()? {
      let Some(need) = self.key_need(conversation)? else {
        self.forget_key_reask(conversation)?;
        continue;
      };
      let Some(record) = self.key_reask_record(conversation)? else {
        // Seen for the first time: the bounded wait starts now, also on a new connection.
        self.save_key_reask(&KeyReaskRecord {
          conversation,
          since_ms: now,
          maybe_asked_at_ms: None,
          attempts: 0,
        })?;
        continue;
      };
      if !self.is_reask_due(&record, trigger, now) {
        continue;
      }
      // Noted first: a failed post waits for the next turn like an unanswered one.
      self.save_key_reask(&KeyReaskRecord {
        maybe_asked_at_ms: Some(now),
        attempts: record.attempts.saturating_add(1),
        ..record
      })?;
      let result = match need {
        KeyNeed::CurrentKey => self.post_current_key_request(conversation).await,
        KeyNeed::Group => self.post_device_add_request(conversation).await,
      };
      match result {
        Ok(true) => {
          tracing::info!(conversation_id = %conversation, ?need, attempts = record.attempts.saturating_add(1), "asked the members again for what this device needs to write");
          posted += 1;
        },
        Ok(false) => {},
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, %error, "could not ask the members again");
          maybe_first_error.get_or_insert(error);
        },
      }
    }
    maybe_first_error.map_or(Ok(posted), Err)
  }

  /// A device of the group asked for the current key (a hand-over to it may have been lost): queues
  /// handing it over like for a further device's join, ranked so normally one member sends. Only
  /// for a device in this device's group; a repeat within `key_reask_retry` (a device asking often,
  /// a server replaying entries) costs nothing.
  pub(crate) fn note_current_key_request(
    &mut self,
    conversation: ConversationId,
    device: DeviceId,
  ) -> Result<(), CoreError> {
    if device == self.account.device_id || !self.has_group(conversation)? {
      return Ok(());
    }
    let (now, window_ms) = (self.now_ms(), self.key_reask_retry_ms());
    let is_repeated = self
      .current_key_requests_handled
      .get(&(conversation, device))
      .is_some_and(|handled_at| is_within(*handled_at, now, window_ms));
    if is_repeated {
      tracing::debug!(conversation_id = %conversation, device_id = %device, "skipped a repeated current-key request");
      return Ok(());
    }
    if !self.group_device_ids(conversation)?.contains(&device) {
      // Only the group's own devices get the key; the request is the server's word for its sender.
      tracing::warn!(conversation_id = %conversation, "ignored a current-key request from a device outside the group");
      return Ok(());
    }
    self
      .current_key_requests_handled
      .retain(|_, handled_at| is_within(*handled_at, now, window_ms));
    self
      .current_key_requests_handled
      .insert((conversation, device), now);
    self.queue_current_key_share_to(conversation, device)
  }

  /// Conversations where this device may be unable to write: those it noted before, and those
  /// whose outbox holds messages back or that wait for a current key.
  fn reask_candidates(&self) -> Result<BTreeSet<ConversationId>, CoreError> {
    let mut candidates = BTreeSet::new();
    for prefix in [KEY_REASK_PREFIX, HELD_FOR_KEY_PREFIX, CURRENT_KEY_WAIT_PREFIX] {
      let table = if prefix == KEY_REASK_PREFIX {
        Table::SyncState
      } else {
        Table::AppliedChanges
      };
      for (key, _) in self.store.list_prefix(table, prefix)? {
        if let Some(conversation) = key
          .strip_prefix(prefix)
          .and_then(|raw| raw.parse::<ConversationId>().ok())
        {
          candidates.insert(conversation);
        }
      }
    }
    Ok(candidates)
  }

  /// What this device lacks to write in the conversation, if asking the members is the way to get
  /// it. `None` where it can write, isn't a member, or another request of its own covers it (a
  /// rejoin, the setup of a DM it opened), which asks on its own schedule.
  fn key_need(&mut self, conversation: ConversationId) -> Result<Option<KeyNeed>, CoreError> {
    let has_current = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .is_some_and(|ring| ring.current().is_some());
    if has_current || !self.is_member_here(conversation)? {
      return Ok(None);
    }
    if self.has_group(conversation)? {
      return Ok(Some(KeyNeed::CurrentKey));
    }
    // A channel's self-add doesn't cover it: members may refuse that, while a member holding the
    // user's addition still commits it.
    let is_covered = self.has_requested_rejoin(conversation)? || self.has_dm_setup(conversation)?;
    // Outside the group only for a message the user wrote: a first device may be listed in many
    // conversations it reads with the key bag alone.
    if is_covered || !super::member_device::has_sends_held_for_key(&self.store, conversation)? {
      return Ok(None);
    }
    Ok(Some(KeyNeed::Group))
  }

  /// Whether the cached conversation lists this user as a member and is not archived.
  fn is_member_here(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(
      self
        .cached_conversations()?
        .get(&conversation)
        .is_some_and(|response| response.maybe_my_membership.is_some() && !response.is_archived),
    )
  }

  /// Whether the next request is due: the backoff ran out since the last request (or since the
  /// state was first seen), or a new connection came and no request went out within
  /// `key_reask_retry`. A clock set back counts as waited long enough.
  fn is_reask_due(&self, record: &KeyReaskRecord, trigger: ReaskTrigger, now: i64) -> bool {
    let last_ms = record.maybe_asked_at_ms.unwrap_or(record.since_ms);
    if !is_within(last_ms, now, self.key_reask_wait_ms(record.attempts)) {
      return true;
    }
    trigger == ReaskTrigger::NewConnection
      && record
        .maybe_asked_at_ms
        .is_none_or(|asked| !is_within(asked, now, self.key_reask_retry_ms()))
  }

  /// Posts the current-key request into the group's queue at this device's epoch; `Ok(false)` when
  /// the group moved on meanwhile (the next turn asks at the new epoch).
  async fn post_current_key_request(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    let epoch = self.group_or_error(conversation)?.epoch();
    let request = PostMlsMessageRequest {
      device_id: self.account.device_id,
      epoch,
      kind: MlsMessageKind::Proposal,
      payload: Base64Bytes::new(CURRENT_KEY_REQUEST_MAGIC.to_vec()),
    };
    match self.api.post_mls_message(conversation, &request).await {
      Ok(_) => Ok(true),
      Err(error) if error.is_epoch_conflict() => Ok(false),
      Err(error) => Err(error.into()),
    }
  }

  /// How long after its `attempts`-th request this device waits before the next.
  fn key_reask_wait_ms(&self, attempts: u32) -> i64 {
    // The shared backoff (`crate::retry`), without jitter: one device's requests, not a fleet.
    let policy = RetryPolicy::new(self.settings.key_reask_retry, self.settings.key_reask_retry_max);
    millis(policy.backoff(attempts))
  }

  fn key_reask_retry_ms(&self) -> i64 {
    i64::try_from(self.settings.key_reask_retry.as_millis()).unwrap_or(i64::MAX)
  }

  fn key_reask_record(&self, conversation: ConversationId) -> Result<Option<KeyReaskRecord>, CoreError> {
    records::get_json(&self.store, Table::SyncState, &key_reask_key(conversation))
  }

  fn save_key_reask(&self, record: &KeyReaskRecord) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::SyncState,
      &key_reask_key(record.conversation),
      record,
    )
  }

  /// This device can write in the conversation, or no longer has to: the note and its backoff go.
  fn forget_key_reask(&self, conversation: ConversationId) -> Result<(), CoreError> {
    Ok(
      self
        .store
        .delete(Table::SyncState, &key_reask_key(conversation))?,
    )
  }
}

/// Where this device notes that it can't write in a conversation.
fn key_reask_key(conversation: ConversationId) -> String {
  format!("{KEY_REASK_PREFIX}{conversation}")
}
