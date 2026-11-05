//! The directory: paged people and channels with filters, and the invitations a caller may manage
//! (issue #298). Every list answers one page; a client asks for the next with `after`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
  ConversationId, ConversationVisibility, InviteId, PresenceState, SpaceId, SpaceRole, UnixMillis, UserId,
  UserKind, UserSummary,
};

/// Query of `GET /api/v1/directory/people`. Filters combine; bots are never listed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DirectoryPeopleQuery {
  /// Matched against name, username, title and department; empty lists everybody, page by page.
  #[serde(default)]
  pub query: String,
  /// Only members or only guests (the External tab).
  #[serde(rename = "kind", default, skip_serializing_if = "Option::is_none")]
  pub maybe_kind: Option<UserKind>,
  /// Only people of this organization (exact match).
  #[serde(rename = "organization", default, skip_serializing_if = "Option::is_none")]
  pub maybe_organization: Option<String>,
  /// Only people of this department (exact match).
  #[serde(rename = "department", default, skip_serializing_if = "Option::is_none")]
  pub maybe_department: Option<String>,
  /// Only people with this title (exact match).
  #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
  pub maybe_title: Option<String>,
  /// Only members of this space; needs the right to see its roster.
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
  /// The last person of the previous page.
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<UserId>,
}

/// One person of the directory, with the profile facts a card shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectoryPerson {
  pub user: UserSummary,
  #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
  pub maybe_title: Option<String>,
  #[serde(rename = "department", default, skip_serializing_if = "Option::is_none")]
  pub maybe_department: Option<String>,
  /// As of this answer; clients follow people they know through the gateway.
  pub presence: PresenceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectoryPeopleResponse {
  pub people: Vec<DirectoryPerson>,
  /// Absent on the last page.
  #[serde(rename = "next_after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_next_after: Option<UserId>,
}

/// Query of `GET /api/v1/directory/filters`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DirectoryFiltersQuery {
  /// Only values of people of this kind.
  #[serde(rename = "kind", default, skip_serializing_if = "Option::is_none")]
  pub maybe_kind: Option<UserKind>,
  /// Only values of members of this space; needs the right to see its roster.
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
}

/// The values the people filters offer, each list sorted and bounded by the most common ones. Empty
/// for guests, who have no directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectoryFiltersResponse {
  pub organizations: Vec<String>,
  pub departments: Vec<String>,
  pub titles: Vec<String>,
}

/// Query of `GET /api/v1/directory/channels`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DirectoryChannelsQuery {
  /// Matched against the channel's name and topic.
  #[serde(default)]
  pub query: String,
  /// Only channels of this space.
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
  /// The last channel of the previous page.
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<ConversationId>,
}

/// A channel across the caller's spaces: one they are in, or a public one of a space they are in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectoryChannel {
  pub id: ConversationId,
  pub space_id: SpaceId,
  pub space_name: String,
  pub name: String,
  #[serde(rename = "topic", default, skip_serializing_if = "Option::is_none")]
  pub maybe_topic: Option<String>,
  pub visibility: ConversationVisibility,
  pub member_count: u32,
  pub is_member: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectoryChannelsResponse {
  pub channels: Vec<DirectoryChannel>,
  /// Absent on the last page.
  #[serde(rename = "next_after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_next_after: Option<ConversationId>,
}

/// Query of `GET /api/v1/directory/invitations`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DirectoryInvitationsQuery {
  /// Matched against the invited address.
  #[serde(default)]
  pub query: String,
  /// Only invitations to this space.
  #[serde(rename = "space_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_space_id: Option<SpaceId>,
  /// The last invitation of the previous page.
  #[serde(rename = "after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_after: Option<InviteId>,
}

/// An open invitation the caller may manage: everything in a space where they manage members, and
/// the guests they invited themselves where they may still invite guests. Newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectoryInvitation {
  pub id: InviteId,
  pub space_id: SpaceId,
  pub space_name: String,
  pub email: String,
  pub role: SpaceRole,
  pub sponsor: UserSummary,
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  /// Past its expiry by the server's clock: it no longer applies at sign-in.
  pub is_expired: bool,
  /// A UI hint: the server can send mail, the invitation is valid and its space not archived. The
  /// resend route checks again and also limits how often.
  pub can_resend: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DirectoryInvitationsResponse {
  pub invitations: Vec<DirectoryInvitation>,
  /// Absent on the last page.
  #[serde(rename = "next_after", default, skip_serializing_if = "Option::is_none")]
  pub maybe_next_after: Option<InviteId>,
}

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;

  #[test]
  fn people_queries_read_their_filters_and_leave_out_what_is_unset() {
    let parsed: DirectoryPeopleQuery = serde_json::from_value(json!({
      "query": "ann", "kind": "guest", "department": "Sales", "title": "Lead", "after": "42"
    }))
    .unwrap();
    assert_eq!(parsed.maybe_kind, Some(UserKind::Guest));
    assert_eq!(parsed.maybe_department.as_deref(), Some("Sales"));
    assert_eq!(parsed.maybe_title.as_deref(), Some("Lead"));
    assert_eq!(parsed.maybe_after.map(|id| id.to_string()).as_deref(), Some("42"));
    assert_eq!(
      serde_json::to_value(DirectoryPeopleQuery::default()).unwrap(),
      json!({"query": ""})
    );
  }

  #[test]
  fn the_last_page_has_no_next_marker_on_the_wire() {
    let page = DirectoryInvitationsResponse {
      invitations: Vec::new(),
      maybe_next_after: None,
    };
    assert_eq!(serde_json::to_value(page).unwrap(), json!({"invitations": []}));
  }

  #[test]
  fn filters_have_a_stable_shape() {
    assert_eq!(
      serde_json::to_value(DirectoryFiltersResponse::default()).unwrap(),
      json!({"organizations": [], "departments": [], "titles": []})
    );
  }
}
