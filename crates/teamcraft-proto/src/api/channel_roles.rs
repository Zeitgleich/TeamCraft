//! A channel's signed role log (e2ee.md §4.6.1, admin.md §2.3, issue #380).
//!
//! An append-only log per channel, like the workspace roster: each entry is a full snapshot of the
//! channel's admins and moderators, signed by the device of somebody allowed to assign them. The
//! server checks who may append (`manage_channel_roles`) and the ordering, stores the bytes
//! verbatim and serves them. Clients verify the signature, the signer's authority and the chain;
//! the server's own member list is never trusted for a role.

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
