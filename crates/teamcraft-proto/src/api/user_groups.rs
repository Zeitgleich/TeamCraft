//! User groups (`@devteam`): named sets of people that can be mentioned.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{UserGroupId, UserId, UserSummary};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UserGroupResponse {
  pub id: UserGroupId,
  /// What follows the `@`: lower case letters, digits, `-` and `_`.
  pub handle: String,
  pub name: String,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  /// The identity provider owns the members; they cannot be edited here.
  pub is_synced: bool,
  pub member_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListUserGroupsResponse {
  pub groups: Vec<UserGroupResponse>,
}

/// A group and who is in it, as a sender's device needs it to turn `@handle` into the people to
/// notify (group mentions are expanded on the device: the server can't read them, e2ee.md §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UserGroupRoster {
  pub id: UserGroupId,
  pub handle: String,
  pub name: String,
  /// The active accounts in the group; a device only ever uses those in the conversation at hand.
  pub member_ids: Vec<UserId>,
}

/// `GET /api/v1/user-groups/rosters`: every group with its members in one answer, so a device
/// doesn't read them one by one each time it connects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ListUserGroupRostersResponse {
  pub groups: Vec<UserGroupRoster>,
}

/// `GET /api/v1/user-groups/{id}`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UserGroupDetailResponse {
  pub group: UserGroupResponse,
  pub members: Vec<UserSummary>,
}

/// `POST /api/v1/user-groups`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreateUserGroupRequest {
  pub handle: String,
  pub name: String,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
}

/// `PATCH /api/v1/user-groups/{id}`: the handle cannot change (mentions point to it).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdateUserGroupRequest {
  #[serde(rename = "name", default, skip_serializing_if = "Option::is_none")]
  pub maybe_name: Option<String>,
  #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
  pub maybe_description: Option<String>,
  /// Removes the description.
  #[serde(default)]
  pub clears_description: bool,
}
