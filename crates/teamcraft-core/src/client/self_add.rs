//! A channel member whose device isn't in the encryption group asks to be let in with a signed
//! self-add (e2ee.md §4.6). The members' clients decide whether to commit it, and may refuse it
//! (a self-add that asks for more than the history from the join on, e2ee.md §4.7, or a policy
//! that forbids it) or never commit it. Nothing tells the joiner, so it posts the self-add again
//! at a growing interval until it is in the group, and shows that it waits (issue #907).

use serde::{Deserialize, Serialize};
use teamcraft_crypto::membership::{HistoryShare, MembershipAction};
use teamcraft_proto::ConversationId;

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::retry::{RetryPolicy, millis};
use crate::store::{LocalStore, Table};

/// Where this device notes the self-adds it posted, one per conversation.
const SELF_ADD_PREFIX: &str = "self_add/";

/// Whether this device posted a self-add to a conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelfAddState {
  None,
  /// Posted and noted; posted again when due.
  Noted,
  /// Posted once by a version before issue #907, which never posted it again.
  LegacyMark,
}

/// What this device noted when it posted a self-add, kept until it is in the group.
#[derive(Serialize, Deserialize)]
struct SelfAddRecord {
  conversation: ConversationId,
  requested_at_ms: i64,
  /// Self-adds posted so far; the wait before the next one doubles with each.
  attempts: u32,
}

/// Whether this device posted a self-add to the conversation and isn't in its encryption group
/// yet. Read by the UI from the store.
pub fn is_waiting_for_admission(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<bool, CoreError> {
  Ok(
    store
      .get(Table::SyncState, &self_add_key(conversation))?
      .is_some(),
  )
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Posts the signed self-add of this user to a channel the server lists them in, and notes it so
  /// it is posted again if no member lets this device in ([`Self::retry_due_self_adds`]).
  pub(crate) async fn post_self_add_change(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let attempts = self
      .self_add_record(conversation)?
      .map_or(0, |record| record.attempts);
    // Noted first: a failed post waits for the next turn like a refused one.
    records::put_json(
      &self.store,
      Table::SyncState,
      &self_add_key(conversation),
      &SelfAddRecord {
        conversation,
        requested_at_ms: self.now_ms(),
        attempts: attempts.saturating_add(1),
      },
    )?;
    // Nobody chose to show the past to somebody joining on their own: history from the join on
    // (e2ee.md §4.7). Members' clients refuse a self-add that says otherwise.
    self
      .post_admin_change(
        conversation,
        MembershipAction::AddUser {
          user_id: convert::user(self.account.user_id),
          history: HistoryShare::FromJoin,
        },
      )
      .await
      .map(|_| ())
  }

  /// Posts the self-add again to every channel where it is due: this device is still a member on
  /// the server, has no group, and waited `self_add_retry` (doubled with every attempt up to
  /// `self_add_retry_max`). A local read while nothing is due. Returns how many were posted.
  pub async fn retry_due_self_adds(&mut self) -> Result<usize, CoreError> {
    let records: Vec<(String, SelfAddRecord)> =
      records::list_json_with_prefix(&self.store, Table::SyncState, SELF_ADD_PREFIX)?;
    let now = self.now_ms();
    let mut posted = 0;
    for (_, record) in records {
      let conversation = record.conversation;
      if self.has_group(conversation)? || !self.is_listed_member(conversation)? {
        self.forget_self_add(conversation)?;
        continue;
      }
      // A clock set back counts as waited long enough: the retry is never stuck.
      let waited_ms = now.saturating_sub(record.requested_at_ms);
      if (0..self.self_add_wait_ms(record.attempts)).contains(&waited_ms) {
        continue;
      }
      tracing::info!(conversation_id = %conversation, attempts = record.attempts, "no member let this device in; posting the self-add again");
      self.post_self_add_change(conversation).await?;
      posted += 1;
    }
    Ok(posted)
  }

  /// Drops the note of a self-add (and an earlier version's mark): this device is in the group,
  /// or no longer a member.
  pub(crate) fn forget_self_add(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.store.delete(Table::SyncState, &self_add_key(conversation))?;
    self
      .store
      .delete(Table::SyncState, &legacy_self_add_key(conversation))?;
    Ok(())
  }

  /// Whether this device posted a self-add to the conversation before.
  pub(crate) fn self_add_state(&self, conversation: ConversationId) -> Result<SelfAddState, CoreError> {
    if self
      .store
      .get(Table::SyncState, &self_add_key(conversation))?
      .is_some()
    {
      return Ok(SelfAddState::Noted);
    }
    if self
      .store
      .get(Table::SyncState, &legacy_self_add_key(conversation))?
      .is_some()
    {
      return Ok(SelfAddState::LegacyMark);
    }
    Ok(SelfAddState::None)
  }

  fn self_add_record(&self, conversation: ConversationId) -> Result<Option<SelfAddRecord>, CoreError> {
    records::get_json(&self.store, Table::SyncState, &self_add_key(conversation))
  }

  /// Whether the cached conversation lists this user as a member and is not archived.
  fn is_listed_member(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let maybe_response: Option<teamcraft_proto::ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    Ok(maybe_response.is_some_and(|response| response.maybe_my_membership.is_some() && !response.is_archived))
  }

  /// How long after its `attempts`-th self-add this device waits before posting it again.
  fn self_add_wait_ms(&self, attempts: u32) -> i64 {
    // The shared backoff (`crate::retry`), without jitter: one device's requests, not a fleet.
    let policy = RetryPolicy::new(self.settings.self_add_retry, self.settings.self_add_retry_max);
    millis(policy.backoff(attempts.saturating_sub(1)))
  }
}

/// Where this device notes the self-add it posted to a conversation.
pub(crate) fn self_add_key(conversation: ConversationId) -> String {
  format!("{SELF_ADD_PREFIX}{conversation}")
}

/// Where versions before issue #907 marked a channel whose self-add they posted once.
fn legacy_self_add_key(conversation: ConversationId) -> String {
  format!("self-add/{conversation}")
}
