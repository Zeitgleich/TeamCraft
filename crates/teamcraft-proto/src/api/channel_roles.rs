//! A channel's signed role log (e2ee.md §4.6.1, admin.md §2.3, issue #380).
//!
//! An append-only log per channel, like the workspace roster: each entry is a full snapshot of the
//! channel's admins and moderators, signed by the device of somebody allowed to assign them. The
//! server checks who may append (`manage_channel_roles`) and the ordering, stores the bytes
//! verbatim and serves them. Clients verify the signature, the signer's authority and the chain;
//! the server's own member list is never trusted for a role.
//!
//! A channel's signed deletion lives here too (e2ee.md §4.6, issue #1135): verified against these
//! roles and the workspace roster by each device, carried and stored by the server.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{Base64Bytes, ChannelRole, ConversationId, DeviceId, UnixMillis, UserId};

/// Privileged roles one snapshot may hold; members are implicit and never listed.
pub const MAX_CHANNEL_ROLE_ASSIGNMENTS: usize = 500;

/// The document a channel role log entry signs (JSON, the entry's payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelRolesDocument {
  /// Equals the entry's version, so a signed document can't be replayed at another position.
  pub version: u64,
  /// The channel; a document signed for one channel is refused in every other.
  pub conversation_id: ConversationId,
  /// Everybody holding a role above member, each once: the whole state after this entry.
  pub assignments: Vec<ChannelRoleAssignment>,
  /// The hash of the last entry the signer's device accepted (32 zero bytes before the first), so
  /// an entry is bound to the state it was made on.
  pub prev_hash: Base64Bytes,
  /// When the signer's device signed it (its own clock; shown, never used to decide).
  pub signed_at_ms: i64,
  /// Whether this snapshot, or one it builds on, took somebody's role from the server's member list
  /// at a channel's first sight (e2ee.md §4.6.1, "Founding at first sight"): nobody looked at those
  /// roles, so even a workspace admin's signature on it doesn't vouch for them. It never anchors
  /// the channel and is never a checkpoint.
  #[serde(default, skip_serializing_if = "std::ops::Not::not")]
  pub has_server_listed_roles: bool,
}

/// One person's role in a [`ChannelRolesDocument`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelRoleAssignment {
  pub user_id: UserId,
  /// `admin` or `moderator`.
  pub role: ChannelRole,
  /// The person's identity public key as the signer's device pinned it: later entries this person
  /// signs as a channel admin are verified against it, not against the server's directory.
  pub identity_key: Base64Bytes,
}

/// What a signed deletion of a channel signs (JSON, the signed deletion's payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelDeletionDocument {
  /// A document signed for one channel is refused for every other.
  pub conversation_id: ConversationId,
  /// The epoch of the channel's group the signer was told is the current one. The server takes the
  /// deletion only for that epoch, so a device whose group has moved past it refuses it as old.
  pub epoch: u64,
  /// When the signer's device signed it (its own clock): the workspace roster has to be fresh at
  /// this time as well as at the verifying device's.
  pub signed_at_ms: i64,
}

/// A channel's deletion, signed by a device of somebody allowed to delete it. `signature` is a
/// signed log entry of the crypto crate's `WorkspaceLog::ChannelDeletion`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignedChannelDeletion {
  /// A [`ChannelDeletionDocument`] as JSON, verbatim as signed.
  pub payload: Base64Bytes,
  pub signature: Base64Bytes,
}

/// The optional body of `DELETE /api/v1/conversations/{id}`. Clients before signed deletions send
/// none, and neither does somebody whose authority isn't signed (a space admin, a custom scheme):
/// the deletion then stays the server's word on the members' devices (e2ee.md §4.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeleteConversationRequest {
  #[serde(rename = "signed_deletion", default, skip_serializing_if = "Option::is_none")]
  pub maybe_signed_deletion: Option<SignedChannelDeletion>,
}

/// A deleted conversation's signed deletion, for a device that was away.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeletedConversationSignature {
  pub conversation_id: ConversationId,
  pub signed_deletion: SignedChannelDeletion,
}

/// `POST /api/v1/conversations/{id}/role-log`: appends an entry signed by the caller's device.
/// `expected_version` is the newest version the client knows (0 for the first).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AppendChannelRoleLogRequest {
  pub expected_version: u64,
  pub payload: Base64Bytes,
  pub signature: Base64Bytes,
  pub signer_device_id: DeviceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelRoleLogEntryResponse {
  pub version: u64,
  pub payload: Base64Bytes,
  pub signature: Base64Bytes,
  #[serde(rename = "signer_device_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_signer_device_id: Option<DeviceId>,
  /// The server's time of the append; a roster admin's authority is judged at it.
  pub created_at: UnixMillis,
}

/// `GET /api/v1/conversations/{id}/role-log`: entries after a version, oldest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListChannelRoleLogQuery {
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<u64>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListChannelRoleLogResponse {
  pub entries: Vec<ChannelRoleLogEntryResponse>,
  pub has_more: bool,
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Id, Snowflake};

  #[test]
  fn the_signed_document_keeps_its_wire_shape() {
    let document = ChannelRolesDocument {
      version: 2,
      conversation_id: ConversationId::from_snowflake(Snowflake::new(7).unwrap()),
      assignments: vec![ChannelRoleAssignment {
        user_id: UserId::from_snowflake(Snowflake::new(9).unwrap()),
        role: ChannelRole::Moderator,
        identity_key: Base64Bytes::new(vec![1, 2, 3]),
      }],
      prev_hash: Base64Bytes::new(vec![0; 3]),
      signed_at_ms: 1_000,
      has_server_listed_roles: false,
    };
    let json = serde_json::to_value(&document).unwrap();
    assert_eq!(
      json,
      serde_json::json!({
        "version": 2,
        "conversation_id": "7",
        "assignments": [{"user_id": "9", "role": "moderator", "identity_key": "AQID"}],
        "prev_hash": "AAAA",
        "signed_at_ms": 1000,
      })
    );
    assert_eq!(
      serde_json::from_value::<ChannelRolesDocument>(json).unwrap(),
      document
    );
  }

  #[test]
  fn an_unknown_role_survives_parsing_for_the_verifier_to_refuse() {
    let assignment: ChannelRoleAssignment =
      serde_json::from_value(serde_json::json!({"user_id": "9", "role": "owner", "identity_key": ""}))
        .unwrap();
    assert!(matches!(assignment.role, ChannelRole::Unknown(_)));
  }
}

#[cfg(test)]
mod signed_deletion_tests {
  use super::*;
  use crate::{Id, Snowflake};

  #[test]
  fn the_document_and_request_keep_their_wire_format() {
    let document = ChannelDeletionDocument {
      conversation_id: conversation(7),
      epoch: 12,
      signed_at_ms: 1_000,
    };
    assert_eq!(
      serde_json::to_string(&document).unwrap(),
      r#"{"conversation_id":"7","epoch":12,"signed_at_ms":1000}"#
    );
    let request = DeleteConversationRequest {
      maybe_signed_deletion: Some(signed()),
    };
    assert_eq!(
      serde_json::to_string(&request).unwrap(),
      r#"{"signed_deletion":{"payload":"AQ==","signature":"Ag=="}}"#
    );
  }

  /// An unsigned deletion is what clients before signed deletions send, and all an admin without a
  /// signed authority has: the request stays empty on the wire and reads from nothing.
  #[test]
  fn an_unsigned_deletion_is_an_empty_body() {
    assert_eq!(
      serde_json::to_string(&DeleteConversationRequest::default()).unwrap(),
      "{}"
    );
    let parsed: DeleteConversationRequest = serde_json::from_str("{}").unwrap();
    assert!(parsed.maybe_signed_deletion.is_none());
  }

  fn signed() -> SignedChannelDeletion {
    SignedChannelDeletion {
      payload: Base64Bytes::new(vec![1]),
      signature: Base64Bytes::new(vec![2]),
    }
  }

  fn conversation(id: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
