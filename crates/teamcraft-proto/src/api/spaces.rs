use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{CategoryId, IdempotencyKey, InviteId, SpaceId, SpaceRole, UnixMillis, UserId, UserSummary};

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
  /// The space's picture, if it has one (`GET /api/v1/spaces/{id}/image`). Like the name it is
  /// directory data the server sees, not end-to-end encrypted content (e2ee.md §4.4).
  #[serde(rename = "image", default, skip_serializing_if = "Option::is_none")]
  pub maybe_image: Option<SpaceImage>,
  pub join_policy: JoinPolicy,
  pub is_archived: bool,
  /// Brand tag: the space may be themed with this brand's accent color. A hint; it restricts
  /// nothing (docs/branding.md §7).
  #[serde(rename = "brand_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_brand_id: Option<String>,
  /// The caller's role; absent if the caller is not a member (open spaces they could join).
  #[serde(rename = "my_role", default, skip_serializing_if = "Option::is_none")]
  pub maybe_my_role: Option<SpaceRole>,
  /// The caller's notification setting for the whole space; absent for non-members.
  #[serde(rename = "my_notifications", default, skip_serializing_if = "Option::is_none")]
  pub maybe_my_notifications: Option<crate::SpaceNotificationSettings>,
}

/// The picture of a space: a square PNG the server re-encoded from the upload
/// (`PUT /api/v1/spaces/{id}/image`, space admins).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SpaceImage {
  /// Changes with every new picture (a content hash); the image's `ETag` is this value in quotes.
  /// Clients key their cache by it.
  pub version: String,
  pub size_bytes: u64,
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
  /// Client-generated key that makes a retry of this request return the original result instead
  /// of creating a second one. Optional so older clients keep working.
  #[serde(rename = "idempotency_key", default, skip_serializing_if = "Option::is_none")]
  pub maybe_idempotency_key: Option<IdempotencyKey>,
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
  /// The id of a configured brand; an empty string clears the tag.
  #[serde(rename = "brand_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_brand_id: Option<String>,
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

#[cfg(test)]
mod tests {
  use serde_json::json;

  use super::*;
  use crate::{Id, Snowflake};

  #[test]
  fn a_space_without_an_image_has_no_image_field() {
    let space = space(None);
    let value = serde_json::to_value(&space).unwrap();
    assert!(value.get("image").is_none(), "{value}");
    // Older servers send no `image`: it reads as none.
    let parsed: SpaceResponse = serde_json::from_value(json!({
      "id": "7",
      "name": "Project A",
      "join_policy": "open",
      "is_archived": false,
    }))
    .unwrap();
    assert_eq!(parsed, space);
  }

  #[test]
  fn a_space_image_has_a_stable_shape() {
    let space = space(Some(SpaceImage {
      version: "3f2a".into(),
      size_bytes: 1234,
    }));
    let value = serde_json::to_value(&space).unwrap();
    assert_eq!(value["image"], json!({"version": "3f2a", "size_bytes": 1234}));
    assert_eq!(serde_json::from_value::<SpaceResponse>(value).unwrap(), space);
  }

  fn space(maybe_image: Option<SpaceImage>) -> SpaceResponse {
    SpaceResponse {
      id: SpaceId::from_snowflake(Snowflake::new(7).unwrap()),
      name: "Project A".into(),
      maybe_icon: None,
      maybe_color: None,
      maybe_description: None,
      maybe_image,
      join_policy: JoinPolicy::Open,
      is_archived: false,
      maybe_brand_id: None,
      maybe_my_role: None,
      maybe_my_notifications: None,
    }
  }
}
