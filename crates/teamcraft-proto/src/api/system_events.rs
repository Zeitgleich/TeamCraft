//! System events of a conversation: what happened to the channel and its members, written by the
//! server from the audit log (admin.md §4). Metadata only; clients show them as system messages.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{ConversationId, SystemEventId, UnixMillis, UserId};

wire_enum! {
  pub enum SystemEventKind {
    MemberAdded => "member_added",
    MemberRemoved => "member_removed",
    /// The member left on their own.
    MemberLeft => "member_left",
    RoleChanged => "role_changed",
    /// An administrator assigned a new owner to a channel without one.
    OwnerAssigned => "owner_assigned",
    Archived => "archived",
    Unarchived => "unarchived",
    ModerationChanged => "moderation_changed",
    Renamed => "renamed",
    /// A call rang the person and they did not join. `actor` is the caller, `subject` the person.
    MissedCall => "missed_call",
    RetentionChanged => "retention_changed",
    /// The channel became public or private.
    PrivacyChanged => "privacy_changed",
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SystemEventResponse {
  pub id: SystemEventId,
  pub conversation_id: ConversationId,
  pub kind: SystemEventKind,
  #[serde(rename = "actor_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_actor_id: Option<UserId>,
  #[serde(rename = "subject_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_subject_id: Option<UserId>,
  pub created_at: UnixMillis,
}

/// Query of `GET /api/v1/conversations/{id}/system-events`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListSystemEventsQuery {
  /// Continue after this event (ascending IDs).
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<SystemEventId>,
  #[serde(rename = "limit", default, skip_serializing_if = "Option::is_none")]
  pub maybe_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListSystemEventsResponse {
  pub events: Vec<SystemEventResponse>,
  pub has_more: bool,
}
