//! The permission scheme in force (PLAN.md §13): the built-in grants plus the changes admins made
//! at instance scope and per space. A channel can only restrict (`ChannelModeration`), never grant.
//!
//! Changes are held in memory (the server is one process) and written through to the database.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde_json::json;
use teamcraft_db::{OverrideEdit, StoredOverride};
use teamcraft_proto::{
  ALL_PERMISSIONS, ChannelModeration, ConversationId, ConversationKind, OverrideEffect, Permission,
  PermissionScheme, PermissionSchemeResponse, SchemeChange, SchemeOverride, SchemeRole, SchemeRoleResponse,
  SetChannelModerationRequest, SpaceId, SystemEventKind, Target, UpdatePermissionSchemeRequest,
  check_scheme_change,
};

use super::access::{load_space, require};
use super::audit::{self, Target as AuditTarget};
use super::system_events;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MAX_CHANGES_PER_REQUEST: usize = 200;
/// The roles of a scheme, in the order the API lists them.
const ROLES: [SchemeRole; 7] = [
  SchemeRole::Guest,
  SchemeRole::Member,
  SchemeRole::SpaceAdmin,
  SchemeRole::ChannelAdmin,
  SchemeRole::ChannelModerator,
  SchemeRole::Admin,
  SchemeRole::Owner,
];

/// The schemes in force: one for the instance and one per space that has changes.
pub struct SchemeStore {
  inner: RwLock<Schemes>,
}

#[derive(Default)]
struct Schemes {
  instance: Arc<PermissionScheme>,
  spaces: HashMap<SpaceId, Arc<PermissionScheme>>,
  /// The changes made at each scope (`None` is the instance).
  changes: HashMap<Option<SpaceId>, Vec<SchemeChange>>,
}

impl SchemeStore {
  /// The scheme that decides inside the space (or at instance scope for `None`).
  pub fn for_space(&self, maybe_space: Option<SpaceId>) -> Arc<PermissionScheme> {
    let schemes = self.read();
    match maybe_space.and_then(|space| schemes.spaces.get(&space)) {
      Some(scheme) => scheme.clone(),
      None => schemes.instance.clone(),
    }
  }

  /// Rebuilds the schemes from the stored changes.
  pub fn replace_all(&self, stored: &[StoredOverride]) {
    let mut changes: HashMap<Option<SpaceId>, Vec<SchemeChange>> = HashMap::new();
    for row in stored {
      let (Some(role), Some(permission)) = (known_role(&row.role), known_permission(&row.permission)) else {
        // Written by a newer version; keep the built-in grant.
        continue;
      };
      changes.entry(row.maybe_space).or_default().push(SchemeChange {
        role,
        permission,
        effect: if row.is_grant {
          OverrideEffect::Grant
        } else {
          OverrideEffect::Revoke
        },
      });
    }
    let instance = Arc::new(PermissionScheme::default().with_changes(&as_edits(changes.get(&None))));
    let spaces = changes
      .iter()
      .filter_map(|(space, list)| Some(((*space)?, Arc::new(instance.with_changes(&as_edits(Some(list)))))))
      .collect();
    *self.write() = Schemes {
      instance,
      spaces,
      changes,
    };
  }

  fn changes_at(&self, maybe_space: Option<SpaceId>) -> Vec<SchemeChange> {
    self.read().changes.get(&maybe_space).cloned().unwrap_or_default()
  }

  fn read(&self) -> std::sync::RwLockReadGuard<'_, Schemes> {
    self
      .inner
      .read()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  fn write(&self) -> std::sync::RwLockWriteGuard<'_, Schemes> {
    self
      .inner
      .write()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

impl Default for SchemeStore {
  fn default() -> Self {
    Self {
      inner: RwLock::new(Schemes::default()),
    }
  }
}

/// Loads the stored changes (at startup).
pub async fn load_scheme(state: &AppState) -> Result<(), ApiError> {
  let stored = state.database.list_permission_overrides().await?;
  state.scheme.replace_all(&stored);
  Ok(())
}

/// The instance scheme with its changes.
pub async fn get_instance_scheme(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<PermissionSchemeResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  Ok(response(state, None))
}

/// Applies changes to the instance scheme. Instance admins only.
pub async fn update_instance_scheme(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: UpdatePermissionSchemeRequest,
) -> Result<PermissionSchemeResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  apply(state, caller, None, request).await?;
  Ok(response(state, None))
}

/// The scheme of a space: the instance scheme with the space's changes.
pub async fn get_space_scheme(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
) -> Result<PermissionSchemeResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  load_space(state, caller, space).await?;
  Ok(response(state, Some(space)))
}

pub async fn update_space_scheme(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  request: UpdatePermissionSchemeRequest,
) -> Result<PermissionSchemeResponse, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  load_space(state, caller, space).await?;
  apply(state, caller, Some(space), request).await?;
  Ok(response(state, Some(space)))
}

/// What members and guests may not do in the channel, as set by its admins or an instance admin.
pub async fn get_channel_moderation(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<ChannelModeration, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  Ok(channel_of(state, caller, conversation).await?.moderation)
}

/// Sets the moderation toggles of a channel without being a member.
pub async fn set_channel_moderation(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
  request: SetChannelModerationRequest,
) -> Result<ChannelModeration, ApiError> {
  require(state, caller, Permission::ManageWorkspace, &Target::Instance)?;
  let channel = channel_of(state, caller, conversation).await?;
  let moderation = request.moderation;
  for permission in moderation.member_denied.iter().chain(&moderation.guest_denied) {
    if !permission.is_channel_moderatable() {
      return Err(ApiError::InvalidRequest(format!(
        "{permission} cannot be restricted per channel"
      )));
    }
  }
  state
    .database
    .update_conversation(
      conversation,
      &teamcraft_db::ConversationChanges {
        maybe_moderation: Some(moderation.clone()),
        ..Default::default()
      },
    )
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "channel_moderation_set",
    Some(AuditTarget::new("conversation", conversation)),
    &json!({}),
  )
  .await?;
  if moderation != channel.moderation {
    system_events::emit(
      state,
      conversation,
      SystemEventKind::ModerationChanged,
      Some(caller.user.id),
      None,
    )
    .await;
  }
  let members = state.database.conversation_member_ids(conversation).await?;
  state.hub.publish(
    members,
    &teamcraft_proto::GatewayEvent::ConversationUpdate {
      conversation_id: conversation,
    },
  );
  Ok(moderation)
}

async fn channel_of(
  state: &AppState,
  caller: &AuthenticatedUser,
  conversation: ConversationId,
) -> Result<teamcraft_db::Conversation, ApiError> {
  let view = state
    .database
    .get_conversation(conversation, caller.user.id)
    .await?
    .filter(|view| view.conversation.kind == ConversationKind::Channel)
    .ok_or_else(ApiError::conversation_not_found)?;
  Ok(view.conversation)
}

async fn apply(
  state: &AppState,
  caller: &AuthenticatedUser,
  maybe_space: Option<SpaceId>,
  request: UpdatePermissionSchemeRequest,
) -> Result<(), ApiError> {
  if request.changes.len() > MAX_CHANGES_PER_REQUEST {
    return Err(ApiError::InvalidRequest("too many changes".into()));
  }
  let mut edits = Vec::with_capacity(request.changes.len());
  for change in &request.changes {
    if matches!(change.effect, OverrideEffect::Unknown(_)) {
      return Err(ApiError::InvalidRequest("unknown effect".into()));
    }
    let is_grant = change.effect == OverrideEffect::Grant;
    check_scheme_change(&change.role, &change.permission, is_grant, maybe_space.is_some())
      .map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
    edits.push(OverrideEdit {
      role: change.role.as_str().to_owned(),
      permission: change.permission.as_str().to_owned(),
      maybe_is_grant: match change.effect {
        OverrideEffect::Grant => Some(true),
        OverrideEffect::Revoke => Some(false),
        _ => None,
      },
    });
  }
  state
    .database
    .edit_permission_overrides(maybe_space, &edits)
    .await?;
  load_scheme(state).await?;
  let target = match maybe_space {
    Some(space) => AuditTarget::new("space", space),
    None => AuditTarget {
      kind: "instance",
      id: 0,
    },
  };
  audit::record(
    state,
    Some(caller.user.id),
    "permission_scheme_changed",
    Some(target),
    &json!({"changes": request.changes.len()}),
  )
  .await?;
  Ok(())
}

/// What the roles may do at the scope, and the changes made there.
fn response(state: &AppState, maybe_space: Option<SpaceId>) -> PermissionSchemeResponse {
  let scheme = state.scheme.for_space(maybe_space);
  let changes = state.scheme.changes_at(maybe_space);
  PermissionSchemeResponse {
    roles: ROLES
      .iter()
      .map(|role| SchemeRoleResponse {
        role: role.clone(),
        granted: scheme.granted_to(role),
        overrides: changes
          .iter()
          .filter(|change| change.role == *role)
          .map(|change| SchemeOverride {
            permission: change.permission.clone(),
            effect: change.effect.clone(),
          })
          .collect(),
      })
      .collect(),
  }
}

fn as_edits(maybe_changes: Option<&Vec<SchemeChange>>) -> Vec<(SchemeRole, Permission, bool)> {
  maybe_changes
    .into_iter()
    .flatten()
    .map(|change| {
      (
        change.role.clone(),
        change.permission.clone(),
        change.effect == OverrideEffect::Grant,
      )
    })
    .collect()
}

fn known_role(name: &str) -> Option<SchemeRole> {
  let role = SchemeRole::from(name.to_owned());
  (!matches!(role, SchemeRole::Unknown(_))).then_some(role)
}

fn known_permission(name: &str) -> Option<Permission> {
  ALL_PERMISSIONS
    .iter()
    .find(|permission| permission.as_str() == name)
    .cloned()
}
