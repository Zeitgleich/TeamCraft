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

use crate::{Base64Bytes, ChannelRole, ConversationId, DeviceId, SpaceId, UnixMillis, UserId};

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
  /// The labelled hash of the name the deleter was shown (e2ee.md §4.6.5, D3): on an anchored
  /// channel a member purges at once only if it is the anchor's name, and a deletion without it
  /// takes the retention. Older clients neither send nor read it.
  #[serde(rename = "name_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name_hash: Option<Base64Bytes>,
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

/// The most channels one space's signed deletion may name; a space with more stays partly claims.
pub const MAX_SPACE_DELETION_CHANNELS: usize = 1_000;

/// What a signed deletion of a space signs (JSON, the payload; e2ee.md §4.6.5, D7, issue #1727):
/// the space, and every channel the deleter was shown in it with its epoch and name. A member's
/// device purges a channel at once only if it is on this list, its anchor places it in this space,
/// confirmed, and the name hash is the anchor's; every other channel stays a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceDeletionDocument {
  pub space_id: SpaceId,
  /// The version of the signed workspace roster that makes the signer an owner or admin; a device
  /// whose newest verified roster is older reads the roster first, and refuses otherwise.
  pub roster_version: u64,
  /// Every channel the deleter was shown in the space, each once.
  pub channels: Vec<SpaceDeletionChannel>,
  /// When the signer's device signed it (its own clock); also the signed entry's version.
  pub signed_at_ms: i64,
}

/// One channel of a [`SpaceDeletionDocument`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceDeletionChannel {
  pub conversation_id: ConversationId,
  /// The epoch of the channel's group the deleter was told is the current one; the server takes
  /// the deletion only if it is, so a device whose group has moved past it refuses it as old.
  pub epoch: u64,
  /// The labelled hash of the name the deleter was shown. On an anchored channel a deletion
  /// without it counts as a mismatch.
  #[serde(rename = "name_hash", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name_hash: Option<Base64Bytes>,
}

/// A space's deletion, signed by a device of a workspace owner or admin. `signature` is a signed
/// log entry of the crypto crate's `WorkspaceLog::SpaceDeletion`, its version the document's
/// `signed_at_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignedSpaceDeletion {
  /// A [`SpaceDeletionDocument`] as JSON, verbatim as signed.
  pub payload: Base64Bytes,
  pub signature: Base64Bytes,
}

/// The optional body of `DELETE /api/v1/spaces/{id}`. Without it (an older client, a space admin,
/// a custom scheme) the deletion stays the server's word on every device (e2ee.md §4.6).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeleteSpaceRequest {
  #[serde(rename = "signed_deletion", default, skip_serializing_if = "Option::is_none")]
  pub maybe_signed_deletion: Option<SignedSpaceDeletion>,
}

/// `GET /api/v1/spaces/{id}/deletion`: what deleting the space takes, for the deleter's
/// confirmation and signature (`delete_space`). The server's word, which members check.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceDeletionPreviewResponse {
  /// The space's channels the caller may see, oldest first.
  pub channels: Vec<SpaceDeletionPreviewChannel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceDeletionPreviewChannel {
  pub conversation_id: ConversationId,
  pub name: String,
  /// The channel's group epoch now (0 before its first commit).
  pub epoch: u64,
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

  /// The mark is signed, so it is part of the wire shape; an entry without it reads as before.
  #[test]
  fn the_mark_of_roles_from_the_servers_list_reads_as_absent_before_it_existed() {
    let json = serde_json::json!({
      "version": 1,
      "conversation_id": "7",
      "assignments": [],
      "prev_hash": "AAA=",
      "signed_at_ms": 1,
    });
    let before: ChannelRolesDocument = serde_json::from_value(json).unwrap();
    assert!(!before.has_server_listed_roles);
    let marked = serde_json::to_value(ChannelRolesDocument {
      has_server_listed_roles: true,
      ..before
    })
    .unwrap();
    assert_eq!(marked["has_server_listed_roles"], serde_json::json!(true));
  }

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

  /// The space's deletion is signed as JSON: its shape is part of what members verify (#1727).
  #[test]
  fn the_signed_space_deletion_keeps_its_wire_shape() {
    let document = SpaceDeletionDocument {
      space_id: SpaceId::from_snowflake(Snowflake::new(3).unwrap()),
      roster_version: 4,
      channels: vec![
        SpaceDeletionChannel {
          conversation_id: ConversationId::from_snowflake(Snowflake::new(7).unwrap()),
          epoch: 5,
          maybe_name_hash: Some(Base64Bytes::new(vec![1, 2, 3])),
        },
        SpaceDeletionChannel {
          conversation_id: ConversationId::from_snowflake(Snowflake::new(8).unwrap()),
          epoch: 0,
          maybe_name_hash: None,
        },
      ],
      signed_at_ms: 1_000,
    };
    let json = serde_json::to_value(&document).unwrap();
    assert_eq!(
      json,
      serde_json::json!({
        "space_id": "3",
        "roster_version": 4,
        "channels": [
          {"conversation_id": "7", "epoch": 5, "name_hash": "AQID"},
          {"conversation_id": "8", "epoch": 0},
        ],
        "signed_at_ms": 1000,
      })
    );
    assert_eq!(
      serde_json::from_value::<SpaceDeletionDocument>(json).unwrap(),
      document
    );
  }

  /// A channel's deletion document doesn't read as a space's, nor the other way round: each side
  /// refuses the other's payload before it looks at the signature.
  #[test]
  fn channel_and_space_deletion_documents_do_not_read_as_each_other() {
    let space = serde_json::json!({"space_id": "3", "roster_version": 1, "channels": [], "signed_at_ms": 1});
    assert!(serde_json::from_value::<ChannelDeletionDocument>(space).is_err());
    let channel = serde_json::json!({"conversation_id": "7", "epoch": 1, "signed_at_ms": 1});
    assert!(serde_json::from_value::<SpaceDeletionDocument>(channel).is_err());
  }

  #[test]
  fn a_space_deletion_request_without_a_body_reads_as_unsigned() {
    let request: DeleteSpaceRequest = serde_json::from_str("{}").unwrap();
    assert_eq!(request, DeleteSpaceRequest::default());
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
    let mut document = ChannelDeletionDocument {
      conversation_id: conversation(7),
      epoch: 12,
      signed_at_ms: 1_000,
      maybe_name_hash: None,
    };
    assert_eq!(
      serde_json::to_string(&document).unwrap(),
      r#"{"conversation_id":"7","epoch":12,"signed_at_ms":1000}"#
    );
    // The name hash (D3) is added only when there is one; older documents read without it.
    document.maybe_name_hash = Some(Base64Bytes::new(vec![3]));
    let json = r#"{"conversation_id":"7","epoch":12,"signed_at_ms":1000,"name_hash":"Aw=="}"#;
    assert_eq!(serde_json::to_string(&document).unwrap(), json);
    assert_eq!(
      serde_json::from_str::<ChannelDeletionDocument>(json).unwrap(),
      document
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
