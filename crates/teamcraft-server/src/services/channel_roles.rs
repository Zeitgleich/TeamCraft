//! Each channel's signed role log (issue #380, e2ee.md §4.6.1, admin.md §2.3).
//!
//! The devices of people who may assign roles append entries; the server keeps the log
//! append-only and ordered, checks who may append and that an entry names this channel and its
//! position, and serves it to whoever may see the members. Verifying signatures and the signer's
//! authority is up to the clients, which never take the member list's role column for a signed
//! role.

use serde_json::json;
use teamcraft_db::{AppendOutcome, ChannelRoleLogEntry, DeletionSignature, StoredDeletionSignature};
use teamcraft_proto::{
  AppendChannelRoleLogRequest, Base64Bytes, ChannelDeletionDocument, ChannelRoleLogEntryResponse,
  ChannelRolesDocument, ConversationId, DeletedConversationSignature, GatewayEvent, ListChannelRoleLogQuery,
  ListChannelRoleLogResponse, MAX_CHANNEL_ROLE_ASSIGNMENTS, Permission, SignedChannelDeletion,
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
/// A signed deletion's document is three short fields.
const MAX_DELETION_PAYLOAD_BYTES: usize = 1024;

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
  match load_conversation(state, caller, id).await {
    // Whoever may see the members and their roles may check that they are signed.
    Ok(access) if !can_list_members(&access, caller) => {
      return Err(ApiError::Forbidden(
        "only members can see the channel's roles".into(),
      ));
    },
    Ok(_) => {},
    // A device that was away verifies a signed deletion against entries it may have missed.
    Err(error) => {
      if !state
        .database
        .can_read_deleted_role_log(caller.user.id, id)
        .await?
      {
        return Err(error);
      }
    },
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

/// The checks of a signed deletion the server can make: sizes, and that its document reads (the
/// conversation, to compare, and the epoch, to compare with the group's). The signature is for the
/// members' devices to verify. `maybe_expected`: the one conversation this request is about.
pub(crate) fn check_signed_deletion(
  signed: &SignedChannelDeletion,
  maybe_expected: Option<ConversationId>,
) -> Result<(ConversationId, DeletionSignature), ApiError> {
  if signed.payload.is_empty() || signed.payload.len() > MAX_DELETION_PAYLOAD_BYTES {
    return Err(ApiError::InvalidRequest(
      "the signed deletion's payload has an invalid size".into(),
    ));
  }
  if signed.signature.is_empty() || signed.signature.len() > MAX_SIGNATURE_BYTES {
    return Err(ApiError::InvalidRequest(
      "the signed deletion's signature has an invalid size".into(),
    ));
  }
  let document: ChannelDeletionDocument = serde_json::from_slice(signed.payload.as_slice())
    .map_err(|e| ApiError::InvalidRequest(format!("the payload is not a deletion document: {e}")))?;
  if maybe_expected.is_some_and(|expected| expected != document.conversation_id) {
    return Err(ApiError::InvalidRequest(
      "the signed deletion is for another channel".into(),
    ));
  }
  Ok((
    document.conversation_id,
    DeletionSignature {
      epoch: document.epoch,
      payload: signed.payload.as_slice().to_vec(),
      signature: signed.signature.as_slice().to_vec(),
    },
  ))
}

pub(crate) fn deletion_signature_response(stored: StoredDeletionSignature) -> DeletedConversationSignature {
  DeletedConversationSignature {
    conversation_id: stored.conversation,
    signed_deletion: SignedChannelDeletion {
      payload: Base64Bytes::new(stored.payload),
      signature: Base64Bytes::new(stored.signature),
    },
  }
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

  mod signed_deletions {
    use super::*;

    #[test]
    fn a_document_for_this_channel_is_taken_with_its_epoch() {
      let (id, kept) = check_signed_deletion(&signed(deletion(CHANNEL, 4), 64), Some(channel())).unwrap();
      assert_eq!((id, kept.epoch), (channel(), 4));
      // A space's request names no one conversation; each signature says which it is for.
      assert!(check_signed_deletion(&signed(deletion(CHANNEL + 1, 4), 64), None).is_ok());
    }

    #[test]
    fn another_channel_junk_and_hostile_sizes_are_refused() {
      for (bad, maybe_expected) in [
        (signed(deletion(CHANNEL + 1, 4), 64), Some(channel())),
        (signed(b"not json".to_vec(), 64), Some(channel())),
        (signed(Vec::new(), 64), Some(channel())),
        (signed(vec![b' '; MAX_DELETION_PAYLOAD_BYTES + 1], 64), None),
        (signed(deletion(CHANNEL, 4), 0), Some(channel())),
        (signed(deletion(CHANNEL, 4), MAX_SIGNATURE_BYTES + 1), None),
      ] {
        assert!(matches!(
          check_signed_deletion(&bad, maybe_expected),
          Err(ApiError::InvalidRequest(_))
        ));
      }
    }

    fn deletion(conversation: i64, epoch: u64) -> Vec<u8> {
      serde_json::to_vec(&ChannelDeletionDocument {
        conversation_id: ConversationId::from_snowflake(Snowflake::new(conversation).unwrap()),
        epoch,
        signed_at_ms: 1,
      })
      .unwrap()
    }

    fn signed(payload: Vec<u8>, signature_bytes: usize) -> SignedChannelDeletion {
      SignedChannelDeletion {
        payload: Base64Bytes::new(payload),
        signature: Base64Bytes::new(vec![0; signature_bytes]),
      }
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
