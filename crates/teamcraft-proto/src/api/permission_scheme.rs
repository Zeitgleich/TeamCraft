//! Admin API of the permission scheme (PLAN.md §13): changes to the built-in grants at instance and
//! space scope, and the channel moderation toggles. A channel can only take permissions away.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{ChannelModeration, Permission, SchemeRole};

wire_enum! {
  /// What a change does to a role's permission.
  pub enum OverrideEffect {
    Grant => "grant",
    Revoke => "revoke",
    /// Drops the change: the permission is what the scope above (or the built-in scheme) says.
    Inherit => "inherit",
  }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SchemeChange {
  pub role: SchemeRole,
  pub permission: Permission,
  pub effect: OverrideEffect,
}

/// `PUT /api/v1/admin/permission-scheme` and `PUT /api/v1/admin/spaces/{id}/permission-scheme`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UpdatePermissionSchemeRequest {
  pub changes: Vec<SchemeChange>,
}

/// A change that is in force at the scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SchemeOverride {
  pub permission: Permission,
  pub effect: OverrideEffect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SchemeRoleResponse {
  pub role: SchemeRole,
  /// What the role may do at this scope, overrides included.
  pub granted: Vec<Permission>,
  /// The changes made at this scope (not the ones inherited from the instance).
  pub overrides: Vec<SchemeOverride>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PermissionSchemeResponse {
  pub roles: Vec<SchemeRoleResponse>,
}

/// `PUT /api/v1/admin/conversations/{id}/moderation`: what members and guests may not do in the channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetChannelModerationRequest {
  pub moderation: ChannelModeration,
}

/// `POST /api/v1/admin/conversations/{id}/owner`: makes an existing member a channel admin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AssignChannelOwnerRequest {
  pub user_id: crate::UserId,
}
