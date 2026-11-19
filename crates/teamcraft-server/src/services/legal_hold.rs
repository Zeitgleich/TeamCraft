//! Legal hold on the server (data-export.md §4.2, admin.md §3.14): conversations the signed policy
//! holds are kept whatever retention, the inactivity policy or a deletion would do, and placing or
//! releasing a hold is written to the audit log in the same transaction as the policy entry.
//!
//! The list comes from the signed policy as stored, read anew for every decision: a deletion is
//! rare, and a hold must count from the moment its policy entry is stored, also on another instance.
//! A deletion reads it in its own transaction, after locking what it deletes, and the append of a
//! policy that places a hold locks the held channels first (issue #2143): a hold and a deletion at
//! the same moment never both win.

use serde_json::json;
use teamcraft_db::NewAuditEntry;
use teamcraft_proto::{
  ConversationId, Id, LegalHold, Snowflake, UserId, WorkspacePolicy, legal_hold_changes,
};

use super::workspace::{current_policy, stored_policy};
use crate::error::ApiError;
use crate::state::AppState;

/// The conversations the stored signed policy holds, to leave them out of a list before any lock is
/// taken. Deciding a deletion uses [`held_in`] instead.
pub async fn held_conversations(state: &AppState) -> Result<Vec<ConversationId>, ApiError> {
  let (_, policy, _) = current_policy(state).await?;
  Ok(held_by(&policy))
}

/// Whether `conversation` is held by the newest stored policy, given as the payload a deletion read
/// in its transaction after locking what it deletes (`teamcraft_db`'s `*_locked` deletions). A hold
/// appended at the same moment is either in that payload or refused for the deleted channel
/// (issue #2143).
pub fn is_held_in(maybe_policy: Option<&[u8]>, conversation: ConversationId) -> Result<bool, ApiError> {
  Ok(held_in(maybe_policy)?.contains(&conversation))
}

/// The conversations held by the newest stored policy, as [`is_held_in`] reads it.
pub fn held_in(maybe_policy: Option<&[u8]>) -> Result<Vec<ConversationId>, ApiError> {
  match maybe_policy {
    Some(payload) => Ok(held_by(&stored_policy(payload)?)),
    None => Ok(Vec::new()),
  }
}

/// The channels a new policy places a hold on, for the append to lock and check in its transaction:
/// a hold on anything but an existing channel is refused ([`not_a_channel`]). Direct messages,
/// groups and meeting chats are removed by paths a hold can't stop (account deletion, meetings), so a
/// hold on them would promise what the server doesn't keep. Holds already in `before` stay valid.
pub fn new_holds(before: &WorkspacePolicy, after: &WorkspacePolicy) -> Vec<ConversationId> {
  let (placed, _) = legal_hold_changes(
    &before.compliance_archive.legal_holds,
    &after.compliance_archive.legal_holds,
  );
  placed.iter().map(|hold| hold.conversation_id).collect()
}

/// The refusal of a hold on a conversation that is not an existing channel.
pub fn not_a_channel() -> ApiError {
  ApiError::InvalidRequest("compliance_archive: a legal hold must name an existing channel".into())
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
  placed
    .into_iter()
    .map(|hold| ("legal_hold_placed", hold))
    .chain(released.into_iter().map(|hold| ("legal_hold_released", hold)))
    .map(|(action, hold)| {
      let id = state
        .ids
        .next_snowflake()
        .map_err(|error| ApiError::Internal(error.to_string()))?;
      Ok(AuditRow {
        id,
        actor,
        action,
        conversation: hold.conversation_id,
        detail: details(hold, version).to_string(),
      })
    })
    .collect()
}

/// An audit row owned until the append borrows it as a [`NewAuditEntry`].
pub struct AuditRow {
  id: Snowflake,
  actor: UserId,
  action: &'static str,
  conversation: ConversationId,
  detail: String,
}

impl AuditRow {
  pub fn entry(&self) -> NewAuditEntry<'_> {
    NewAuditEntry {
      id: self.id,
      maybe_actor: Some(self.actor),
      action: self.action,
      maybe_target_kind: Some("conversation"),
      maybe_target_id: Some(self.conversation.snowflake().get()),
      detail: &self.detail,
    }
  }
}

fn details(hold: &LegalHold, version: u64) -> serde_json::Value {
  json!({"reference": hold.reference, "placed_at": hold.placed_at.0, "policy_version": version})
}

fn held_by(policy: &WorkspacePolicy) -> Vec<ConversationId> {
  policy
    .compliance_archive
    .legal_holds
    .iter()
    .map(|hold| hold.conversation_id)
    .collect()
}
