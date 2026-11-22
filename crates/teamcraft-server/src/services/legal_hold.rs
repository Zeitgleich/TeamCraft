//! Legal hold on the server (data-export.md §4.2, admin.md §3.14): conversations the signed policy
//! holds, by name or through a held person (issue #2135), are kept whatever retention, the inactivity
//! policy, a deletion or a member's withdrawal of their own message would do, and placing or
//! releasing a hold is written to the audit log in the same transaction as the policy entry.
//!
//! The list comes from the signed policy as stored, read anew for every decision: a deletion is
//! rare, and a hold must count from the moment its policy entry is stored, also on another instance.
//! A deletion, and retention for each conversation, reads it in its own transaction after locking
//! what it deletes ([`HoldEvidence`]), and the append of a policy that places a hold locks the held
//! conversations first (issue #2143): a hold and a deletion at the same moment never both win.
//!
//! A withdrawal under a hold marks the message deleted and keeps it; once no hold keeps the
//! conversation, [`apply_pending_deletions`] removes it (after a release, and with every retention
//! run, so a restart or a release on another instance catches up).

use serde_json::json;
use teamcraft_db::{HeldPerson, HoldEvidence, NewAuditEntry, Withdrawal};
use teamcraft_proto::{
  ClientMessageId, ConversationId, Id, LegalHold, PersonHoldScope, PersonLegalHold, Snowflake, UserId,
  WorkspacePolicy, legal_hold_changes, person_hold_changes,
};
use tracing::warn;

use super::audit::{self, Target as AuditTarget};
use super::workspace::{current_policy, stored_policy};
use crate::error::ApiError;
use crate::state::AppState;

/// Conversations with pending deletions one run looks at; the next run takes the rest.
const PENDING_DELETIONS_BATCH: i64 = 500;

/// The conversations held now, by name or through a held person, to leave them out of a list before
/// any lock is taken. Deciding a deletion uses [`held_in`] instead.
pub async fn held_conversations(state: &AppState) -> Result<Vec<ConversationId>, ApiError> {
  let (_, policy, _) = current_policy(state).await?;
  let mut held = held_by(&policy);
  held.extend(state.database.conversations_covered_by_people().await?);
  held.sort_unstable();
  held.dedup();
  Ok(held)
}

/// The conversations the stored policy holds by name, without those a person hold covers: the leftover
/// sweep of direct messages leaves the covered ones out in its own query.
pub async fn held_by_name(state: &AppState) -> Result<Vec<ConversationId>, ApiError> {
  let (_, policy, _) = current_policy(state).await?;
  Ok(held_by(&policy))
}

/// Whether `conversation` is held, by the holds a deletion read in its transaction after locking
/// what it deletes (`teamcraft_db`'s `*_locked` deletions): by name in the newest stored policy, or
/// through a held person. A hold appended at the same moment is either in them or refused for the
/// deleted conversation (issue #2143).
pub fn is_held_in(evidence: &HoldEvidence, conversation: ConversationId) -> Result<bool, ApiError> {
  Ok(held_in(evidence)?.contains(&conversation))
}

/// The conversations held by the holds a deletion read, as [`is_held_in`] reads them: of those it
/// asked about, the ones held through a person, and all held by name.
pub fn held_in(evidence: &HoldEvidence) -> Result<Vec<ConversationId>, ApiError> {
  let mut held = match &evidence.maybe_policy {
    Some(payload) => held_by(&stored_policy(payload)?),
    None => Vec::new(),
  };
  held.extend(evidence.covered_by_people.iter().copied());
  Ok(held)
}

/// The conversations a new policy places a hold on by name, for the append to lock and check in its
/// transaction: a hold on anything but an existing channel, direct message or group is refused
/// ([`not_a_channel`]). Meeting chats go with their meeting, which a hold can't stop. Holds already
/// in `before` stay valid.
pub fn new_holds(before: &WorkspacePolicy, after: &WorkspacePolicy) -> Vec<ConversationId> {
  let (placed, _) = legal_hold_changes(
    &before.compliance_archive.legal_holds,
    &after.compliance_archive.legal_holds,
  );
  placed.iter().map(|hold| hold.conversation_id).collect()
}

/// The people a policy holds, as the server keeps them beside the entry (issue #2135).
pub fn held_people(policy: &WorkspacePolicy) -> Vec<HeldPerson> {
  policy
    .compliance_archive
    .person_holds
    .iter()
    .map(|hold| HeldPerson {
      user: hold.user_id,
      is_direct_only: hold.scope == PersonHoldScope::Direct,
    })
    .collect()
}

/// The refusal of a hold on a conversation that is not an existing channel, direct message or group.
pub fn not_a_channel() -> ApiError {
  ApiError::InvalidRequest(
    "compliance_archive: a legal hold must name an existing channel, direct message or group".into(),
  )
}

/// The refusal of a hold on an unknown account.
pub fn unknown_person() -> ApiError {
  ApiError::InvalidRequest("compliance_archive: a legal hold names an unknown person".into())
}

/// A member withdraws their own message (`deletes` of a sent message): its ciphertext goes, or, while
/// a hold keeps the conversation, it is marked deleted and kept (data-export.md §4.2). Called for
/// every send that names one, also a repeated one, so a withdrawal a crash interrupted completes
/// with the client's retry.
pub async fn withdraw_message(
  state: &AppState,
  conversation: ConversationId,
  sender: UserId,
  target: ClientMessageId,
) -> Result<(), ApiError> {
  let withdrawal = state
    .database
    .withdraw_message_locked(conversation, sender, target, |evidence| {
      is_held_in(evidence, conversation)
    })
    .await?;
  if let Withdrawal::Deleted {
    maybe_object_key: Some(key),
  } = withdrawal
    && let Some(store) = &state.maybe_blob_store
  {
    super::storage_cleanup::delete_or_queue(state, store, &key).await;
  }
  Ok(())
}

/// Removes the messages members withdrew while a hold kept their conversation, where no hold keeps
/// it any more. Run after a policy entry that releases a hold, and with every retention run. Each
/// conversation is decided under its lock; one failing doesn't stop the others. Returns how many
/// messages went.
pub async fn apply_pending_deletions(state: &AppState) -> Result<u64, ApiError> {
  let mut applied = 0;
  for conversation in state
    .database
    .conversations_with_pending_deletions(PENDING_DELETIONS_BATCH)
    .await?
  {
    let outcome = state
      .database
      .apply_pending_deletions_locked(conversation, |evidence| is_held_in(evidence, conversation))
      .await;
    let done = match outcome {
      Ok(Some(done)) => done,
      Ok(None) => continue,
      Err(error) => {
        warn!(conversation_id = %conversation, %error, "applying held deletions failed");
        continue;
      },
    };
    if let Some(store) = &state.maybe_blob_store {
      for key in &done.object_keys {
        super::storage_cleanup::delete_or_queue(state, store, key).await;
      }
    }
    applied += done.messages;
    audit::record(
      state,
      None,
      "legal_hold_deletions_applied",
      Some(AuditTarget::new("conversation", conversation)),
      &json!({"messages": done.messages}),
    )
    .await?;
  }
  Ok(applied)
}

/// The audit rows for every hold a new policy places or releases, with the signing owner as the
/// actor, to be written in the same transaction as the policy entry. Metadata only: the
/// conversation, the reference and the policy version.
pub fn audit_rows(
  state: &AppState,
  actor: UserId,
  before: &WorkspacePolicy,
  after: &WorkspacePolicy,
  version: u64,
) -> Result<Vec<AuditRow>, ApiError> {
  let (placed, released) = legal_hold_changes(
    &before.compliance_archive.legal_holds,
    &after.compliance_archive.legal_holds,
  );
  let (people_placed, people_released) = person_hold_changes(
    &before.compliance_archive.person_holds,
    &after.compliance_archive.person_holds,
  );
  let conversations = placed
    .into_iter()
    .map(|hold| ("legal_hold_placed", hold))
    .chain(released.into_iter().map(|hold| ("legal_hold_released", hold)))
    .map(|(action, hold)| {
      (
        action,
        "conversation",
        hold.conversation_id.snowflake(),
        details(hold, version),
      )
    });
  let people = people_placed
    .into_iter()
    .map(|hold| ("legal_hold_placed", hold))
    .chain(
      people_released
        .into_iter()
        .map(|hold| ("legal_hold_released", hold)),
    )
    .map(|(action, hold)| {
      (
        action,
        "user",
        hold.user_id.snowflake(),
        person_details(hold, version),
      )
    });
  conversations
    .chain(people)
    .map(|(action, target_kind, target, detail)| {
      let id = state
        .ids
        .next_snowflake()
        .map_err(|error| ApiError::Internal(error.to_string()))?;
      Ok(AuditRow {
        id,
        actor,
        action,
        target_kind,
        target,
        detail: detail.to_string(),
      })
    })
    .collect()
}

/// An audit row owned until the append borrows it as a [`NewAuditEntry`].
pub struct AuditRow {
  id: Snowflake,
  actor: UserId,
  action: &'static str,
  /// `conversation` or `user`.
  target_kind: &'static str,
  target: Snowflake,
  detail: String,
}

impl AuditRow {
  /// Whether this row records a released hold: the append then applies pending deletions.
  pub fn is_release(&self) -> bool {
    self.action == "legal_hold_released"
  }

  pub fn entry(&self) -> NewAuditEntry<'_> {
    NewAuditEntry {
      id: self.id,
      maybe_actor: Some(self.actor),
      action: self.action,
      maybe_target_kind: Some(self.target_kind),
      maybe_target_id: Some(self.target.get()),
      detail: &self.detail,
    }
  }
}

fn details(hold: &LegalHold, version: u64) -> serde_json::Value {
  json!({"reference": hold.reference, "placed_at": hold.placed_at.0, "policy_version": version})
}

fn person_details(hold: &PersonLegalHold, version: u64) -> serde_json::Value {
  json!({
    "reference": hold.reference,
    "scope": hold.scope.as_str(),
    "placed_at": hold.placed_at.0,
    "policy_version": version,
  })
}

fn held_by(policy: &WorkspacePolicy) -> Vec<ConversationId> {
  policy
    .compliance_archive
    .legal_holds
    .iter()
    .map(|hold| hold.conversation_id)
    .collect()
}
