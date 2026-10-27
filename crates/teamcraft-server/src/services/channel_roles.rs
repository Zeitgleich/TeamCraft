//! Each channel's signed role log (issue #380, e2ee.md §4.6.1, admin.md §2.3).
//!
//! The devices of people who may assign roles append entries; the server keeps the log
//! append-only and ordered, checks who may append and that an entry names this channel and its
//! position, and serves it to whoever may see the members. Verifying signatures and the signer's
//! authority is up to the clients, which never take the member list's role column for a signed
//! role.

use serde_json::json;
use teamcraft_db::{AppendOutcome, ChannelRoleLogEntry};
use teamcraft_proto::{
  AppendChannelRoleLogRequest, Base64Bytes, ChannelRoleLogEntryResponse, ChannelRolesDocument,
  ConversationId, GatewayEvent, ListChannelRoleLogQuery, ListChannelRoleLogResponse,
  MAX_CHANNEL_ROLE_ASSIGNMENTS, Permission,
};

use super::access::{load_conversation, require};
use super::audit::{self, Target as AuditTarget};
use super::conversations::{can_list_members, ensure_open_channel};
use super::convert::unix_millis;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// A snapshot of [`MAX_CHANNEL_ROLE_ASSIGNMENTS`] people with their keys stays well below this.
const MAX_PAYLOAD_BYTES: usize = 128 * 1024;
const MAX_SIGNATURE_BYTES: usize = 4096;

pub async fn append(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  request: AppendChannelRoleLogRequest,
) -> Result<ChannelRoleLogEntryResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  ensure_open_channel(&access)?;
  require(state, caller, Permission::ManageChannelRoles, &access.target())?;
  if request.payload.is_empty() || request.payload.len() > MAX_PAYLOAD_BYTES {
    return Err(ApiError::InvalidRequest("payload has an invalid size".into()));
  }
  if request.signature.is_empty() || request.signature.len() > MAX_SIGNATURE_BYTES {
    return Err(ApiError::InvalidRequest("signature has an invalid size".into()));
  }
  if !state
    .database
    .is_active_device_of(caller.user.id, request.signer_device_id)
    .await?
  {
    return Err(ApiError::device_not_found());
  }
  check_payload(request.payload.as_slice(), id, request.expected_version + 1)?;

  let outcome = state
    .database
    .append_channel_role_log(
      id,
      request.expected_version,
      request.payload.as_slice(),
      request.signature.as_slice(),
      request.signer_device_id,
    )
    .await?;
  if let AppendOutcome::VersionConflict { current_version } = outcome {
    return Err(ApiError::VersionConflict { current_version });
  }
  let version = request.expected_version + 1;
  audit::record(
    state,
    Some(caller.user.id),
    "channel_roles_signed",
    Some(AuditTarget::new("conversation", id)),
    &json!({"version": version}),
  )
  .await?;
  let members = state.database.conversation_member_ids(id).await?;
  state.hub.publish(
    members,
    &GatewayEvent::ChannelRolesUpdate {
      conversation_id: id,
      version,
    },
  );
  let entries = state.database.list_channel_role_log(id, version - 1, 1).await?;
  entries
    .first()
    .map(entry_response)
    .ok_or_else(|| ApiError::Internal("appended entry missing".into()))
}

pub async fn list(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  query: ListChannelRoleLogQuery,
) -> Result<ListChannelRoleLogResponse, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  // Whoever may see the members and their roles may check that they are signed.
  if !can_list_members(&access, caller) {
    return Err(ApiError::Forbidden(
      "only members can see the channel's roles".into(),
    ));
  }
  let limits = &state.settings.get().limits;
  let limit = query
    .maybe_limit
    .unwrap_or(limits.default_page_size)
    .clamp(1, limits.max_page_size);
  let mut rows = state
    .database
    .list_channel_role_log(id, query.maybe_after.unwrap_or(0), i64::from(limit))
    .await?;
  let has_more = rows.len() > limit as usize;
  rows.truncate(limit as usize);
  Ok(ListChannelRoleLogResponse {
    entries: rows.iter().map(entry_response).collect(),
    has_more,
  })
}

/// The payload must be a role snapshot of this channel for the entry's position; anything else
/// would only be skipped by every client.
fn check_payload(payload: &[u8], id: ConversationId, expected_new_version: u64) -> Result<(), ApiError> {
  let document: ChannelRolesDocument = serde_json::from_slice(payload)
    .map_err(|e| ApiError::InvalidRequest(format!("the payload is not a channel role document: {e}")))?;
  if document.conversation_id != id {
    return Err(ApiError::InvalidRequest(
      "the document is for another channel".into(),
    ));
  }
  if document.version != expected_new_version {
    return Err(ApiError::InvalidRequest(format!(
      "the document says version {}, the entry would be version {expected_new_version}",
      document.version
    )));
  }
  if document.assignments.len() > MAX_CHANNEL_ROLE_ASSIGNMENTS {
    return Err(ApiError::InvalidRequest("too many role assignments".into()));
  }
  Ok(())
}

fn entry_response(entry: &ChannelRoleLogEntry) -> ChannelRoleLogEntryResponse {
  ChannelRoleLogEntryResponse {
    version: entry.version,
    payload: Base64Bytes::new(entry.payload.clone()),
    signature: Base64Bytes::new(entry.signature.clone()),
    maybe_signer_device_id: entry.maybe_signer,
    created_at: unix_millis(entry.created_at),
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ChannelRole, ChannelRoleAssignment, Id, Snowflake, UserId};

  use super::*;

  const CHANNEL: i64 = 7;

  #[test]
  fn a_snapshot_of_this_channel_at_the_next_version_is_taken() {
    assert!(check_payload(&payload(CHANNEL, 3, 1), channel(), 3).is_ok());
  }

  #[test]
  fn other_channels_versions_and_non_documents_are_refused() {
    for (payload, version) in [
      (payload(CHANNEL + 1, 3, 1), 3),
      (payload(CHANNEL, 2, 1), 3),
      (b"{}".to_vec(), 3),
      (payload(CHANNEL, 3, MAX_CHANNEL_ROLE_ASSIGNMENTS + 1), 3),
    ] {
      assert!(matches!(
        check_payload(&payload, channel(), version),
        Err(ApiError::InvalidRequest(_))
      ));
    }
  }

  fn channel() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(CHANNEL).unwrap())
  }

  fn payload(conversation: i64, version: u64, assignments: usize) -> Vec<u8> {
    let document = ChannelRolesDocument {
      version,
      conversation_id: ConversationId::from_snowflake(Snowflake::new(conversation).unwrap()),
      assignments: (0..assignments)
        .map(|index| ChannelRoleAssignment {
          user_id: UserId::from_snowflake(Snowflake::new(i64::try_from(index).unwrap() + 1).unwrap()),
          role: ChannelRole::Admin,
          identity_key: Base64Bytes::new(vec![0; 32]),
        })
        .collect(),
      prev_hash: Base64Bytes::new(vec![0; 32]),
      signed_at_ms: 0,
    };
    serde_json::to_vec(&document).unwrap()
  }
}
