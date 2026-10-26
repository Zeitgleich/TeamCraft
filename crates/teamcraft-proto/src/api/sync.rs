use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ConversationId, MessageId, MlsMessageId, SpaceId};

/// `GET /api/v1/sync/heads` – where each of the caller's conversations and spaces stands, in one
/// answer, so a client coming back after the resume window reads only the conversations that
/// changed instead of every member list, MLS queue and history (issue #714). A client compares
/// each head with the one it caught up to last time; an equal head needs no request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SyncHeadsResponse {
  /// Every conversation the caller is a member of.
  pub conversations: Vec<ConversationHead>,
  /// Every space the caller may list the members of (not for guests).
  pub spaces: Vec<SpaceHead>,
}

/// The newest entries of one conversation the caller can read, and a digest of its member list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ConversationHead {
  pub conversation_id: ConversationId,
  /// The newest message the caller's history reaches (`GET …/messages` lists it); absent when
  /// there is none.
  #[serde(rename = "last_message_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_last_message_id: Option<MessageId>,
  /// The newest entry of the MLS queue the caller reads (`GET …/mls`, welcomes only to the
  /// caller's own devices); absent when there is none.
  #[serde(
    rename = "last_mls_message_id",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_last_mls_message_id: Option<MlsMessageId>,
  /// Changes whenever `GET …/members` would answer differently (people, roles, names). Opaque;
  /// only compared for equality.
  pub members_digest: String,
}

/// A digest of one space's member list (`GET /spaces/{id}/members`), as for conversations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceHead {
  pub space_id: SpaceId,
  pub members_digest: String,
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::{Id, Snowflake};

  #[test]
  fn a_head_without_entries_leaves_the_ids_out() {
    let head = ConversationHead {
      conversation_id: ConversationId::from_snowflake(Snowflake::new(7).unwrap()),
      maybe_last_message_id: None,
      maybe_last_mls_message_id: Some(MlsMessageId::from_snowflake(Snowflake::new(9).unwrap())),
      members_digest: "ab".into(),
    };
    let value = serde_json::to_value(&head).unwrap();
    assert_eq!(
      value,
      json!({ "conversation_id": "7", "last_mls_message_id": "9", "members_digest": "ab" })
    );
    assert_eq!(serde_json::from_value::<ConversationHead>(value).unwrap(), head);
  }
}
