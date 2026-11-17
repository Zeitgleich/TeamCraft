//! Legal hold on the server (data-export.md §4.2, admin.md §3.14): conversations the signed policy
//! holds are kept whatever retention, the inactivity policy or a deletion would do, and placing or
//! releasing a hold is written to the audit log.
//!
//! The list comes from the signed policy as stored, read anew for every decision: a deletion is
//! rare, and a hold must count from the moment its policy entry is stored, also on another instance.

use serde_json::json;
use teamcraft_proto::{ConversationId, LegalHold, UserId, WorkspacePolicy, legal_hold_changes};

use super::audit::{self, Target as AuditTarget};
use super::workspace::current_policy;
use crate::error::ApiError;
use crate::state::AppState;

/// Refuses with `legal_hold` when the signed policy holds `conversation`.
pub async fn refuse_if_held(state: &AppState, conversation: ConversationId) -> Result<(), ApiError> {
  refuse_if_any_held(state, std::iter::once(conversation)).await
}

/// Refuses with `legal_hold` when the signed policy holds any of `conversations` (a space's channels).
pub async fn refuse_if_any_held(
  state: &AppState,
  conversations: impl IntoIterator<Item = ConversationId>,
) -> Result<(), ApiError> {
  let (_, policy, _) = current_policy(state).await?;
  let holds = &policy.compliance_archive.legal_holds;
  if holds.is_empty() {
    return Ok(());
  }
  if conversations
    .into_iter()
    .any(|conversation| teamcraft_proto::legal_hold_of(holds, conversation).is_some())
  {
    return Err(ApiError::LegalHold);
  }
  Ok(())
}

/// Writes an audit entry for every hold a new policy places or releases, with the signing owner as
/// the actor. Metadata only: the conversation, the reference and the policy version.
pub async fn record_changes(
  state: &AppState,
  actor: UserId,
  before: &WorkspacePolicy,
  after: &WorkspacePolicy,
  version: u64,
) -> Result<(), ApiError> {
  let (placed, released) = legal_hold_changes(
    &before.compliance_archive.legal_holds,
    &after.compliance_archive.legal_holds,
  );
  for (action, hold) in placed
    .into_iter()
    .map(|hold| ("legal_hold_placed", hold))
    .chain(released.into_iter().map(|hold| ("legal_hold_released", hold)))
  {
    audit::record(
      state,
      Some(actor),
      action,
      Some(AuditTarget::new("conversation", hold.conversation_id)),
      &details(hold, version),
    )
    .await?;
  }
  Ok(())
}

fn details(hold: &LegalHold, version: u64) -> serde_json::Value {
  json!({"reference": hold.reference, "placed_at": hold.placed_at.0, "policy_version": version})
}
