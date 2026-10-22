use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{CategoryId, InviteId, SpaceId, SpaceRole, UnixMillis, UserId, UserSummary};

wire_enum! {
  /// How people join a space.
  pub enum JoinPolicy {
    /// Every employee can join.
    Open => "open",
    InviteOnly => "invite_only",
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceResponse {
  pub id: SpaceId,
  pub name: String,
  #[serde(rename = "icon", default, skip_serializing_if = "Option::is_none")]
  pub maybe_icon: Option<String>,
  #[serde(rename = "color", default, skip_serializing_if = "Option::is_none")]
  pub maybe_color: Option<String>,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  pub join_policy: JoinPolicy,
  pub is_archived: bool,
  /// The caller's role; absent if the caller is not a member (open spaces they could join).
  #[serde(rename = "my_role", default, skip_serializing_if = "Option::is_none")]
  pub maybe_my_role: Option<SpaceRole>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListSpacesResponse {
  pub spaces: Vec<SpaceResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateSpaceRequest {
  pub name: String,
  #[serde(rename = "icon", default, skip_serializing_if = "Option::is_none")]
  pub maybe_icon: Option<String>,
  #[serde(rename = "color", default, skip_serializing_if = "Option::is_none")]
  pub maybe_color: Option<String>,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  pub join_policy: JoinPolicy,
}

/// `PATCH /api/v1/spaces/{id}`: only the fields that are present change; an empty string clears
/// icon, color and description.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateSpaceRequest {
  #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name: Option<String>,
  #[serde(rename = "icon", default, skip_serializing_if = "Option::is_none")]
  pub maybe_icon: Option<String>,
  #[serde(rename = "color", default, skip_serializing_if = "Option::is_none")]
  pub maybe_color: Option<String>,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  #[serde(rename = "join_policy", default, skip_serializing_if = "Option::is_none")]
  pub maybe_join_policy: Option<JoinPolicy>,
  #[serde(rename = "is_archived", default, skip_serializing_if = "Option::is_none")]
  pub maybe_is_archived: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceMemberResponse {
  pub user: UserSummary,
  pub role: SpaceRole,
  /// When a guest's membership ends (they are removed automatically).
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  /// The employee responsible for a guest; told when the guest expires.
  #[serde(rename = "sponsor_user_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_sponsor_user_id: Option<UserId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListSpaceMembersResponse {
  pub members: Vec<SpaceMemberResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AddSpaceMemberRequest {
  pub user_id: UserId,
  pub role: SpaceRole,
  /// Guests only; default `[guests] default_expiry_days` from now.
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateSpaceMemberRequest {
  pub role: SpaceRole,
  /// Sets a guest's expiry (extend or shorten).
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
  /// Removes a guest's expiry (needs the same permission as setting it).
  #[serde(default)]
  pub clears_expiry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CategoryResponse {
  pub id: CategoryId,
  pub space_id: SpaceId,
  pub name: String,
  pub position: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListCategoriesResponse {
  pub categories: Vec<CategoryResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateCategoryRequest {
  pub name: String,
  #[serde(rename = "position", default, skip_serializing_if = "Option::is_none")]
  pub maybe_position: Option<i32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateCategoryRequest {
  #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name: Option<String>,
  #[serde(rename = "position", default, skip_serializing_if = "Option::is_none")]
  pub maybe_position: Option<i32>,
}

/// `POST /api/v1/spaces/{id}/invites`: invite a person by email as member or guest. The invite
/// is applied when someone with that verified email signs in; guests can be limited to channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateInviteRequest {
  pub email: String,
  pub role: SpaceRole,
  /// Channels of the space the person joins on acceptance (guests: only these plus nothing else).
  #[serde(default)]
  pub channel_ids: Vec<crate::ConversationId>,
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InviteResponse {
  pub id: InviteId,
  pub space_id: SpaceId,
  pub email: String,
  pub role: SpaceRole,
  pub channel_ids: Vec<crate::ConversationId>,
  /// The employee responsible for the invitee (the inviter).
  pub sponsor_user_id: UserId,
  #[serde(rename = "expires_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_expires_at: Option<UnixMillis>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListInvitesResponse {
  pub invites: Vec<InviteResponse>,
}
