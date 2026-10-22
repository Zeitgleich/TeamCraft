//! Loads what the permission evaluator needs and turns "no" into the right error.
//!
//! Things the caller can't see are reported as *not found* rather than *forbidden*, so private
//! spaces and channels don't leak their existence (ARCHITECTURE.md §5.2).

use teamcraft_db::{ConversationView, SpaceView};
use teamcraft_proto::{
  ChannelContext, ChannelRole, ConversationId, ConversationKind, ConversationVisibility, JoinPolicy,
  Permission, Role, SpaceContext, SpaceId, SpaceRole, Target, UserKind, is_allowed,
};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// A space the caller may know about.
pub struct SpaceAccess {
  pub view: SpaceView,
}

/// A conversation the caller may know about, with the context to evaluate permissions on it.
pub struct ConversationAccess {
  pub view: ConversationView,
  pub maybe_space_role: Option<SpaceRole>,
}

pub fn is_instance_admin(caller: &AuthenticatedUser) -> bool {
  matches!(caller.role, Role::Admin | Role::Owner)
}

pub fn is_guest(caller: &AuthenticatedUser) -> bool {
  caller.user.kind == UserKind::Guest || caller.role == Role::Guest
}

/// `Ok` if the permission is granted on the target, `Forbidden` otherwise.
pub fn require(
  state: &AppState,
  caller: &AuthenticatedUser,
  permission: Permission,
  target: &Target,
) -> Result<(), ApiError> {
  if is_allowed(&state.scheme, &caller.role, &permission, target) {
    Ok(())
  } else {
    Err(ApiError::Forbidden(format!("missing permission {permission}")))
  }
}

pub fn space_target(maybe_role: Option<SpaceRole>) -> Target {
  Target::Space(SpaceContext { maybe_role })
}

impl SpaceAccess {
  pub fn target(&self) -> Target {
    space_target(self.view.maybe_role.clone())
  }
}

impl ConversationAccess {
  pub fn target(&self) -> Target {
    let conversation = &self.view.conversation;
    Target::Channel(ChannelContext {
      maybe_space: conversation.maybe_space_id.map(|_| SpaceContext {
        maybe_role: self.maybe_space_role.clone(),
      }),
      visibility: conversation.visibility.clone(),
      is_archived: conversation.is_archived,
      maybe_role: self.view.maybe_membership.as_ref().map(|m| m.role.clone()),
      moderation: conversation.moderation.clone(),
    })
  }

  pub fn is_member(&self) -> bool {
    self.view.maybe_membership.is_some()
  }

  pub fn role(&self) -> Option<&ChannelRole> {
    self.view.maybe_membership.as_ref().map(|m| &m.role)
  }
}

/// Loads a space the caller may see: a member, an open space (not for guests) or any space for admins.
pub async fn load_space(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
) -> Result<SpaceAccess, ApiError> {
  let view = state
    .database
    .get_space(id, caller.user.id)
    .await?
    .ok_or_else(ApiError::space_not_found)?;
  let is_visible = view.maybe_role.is_some()
    || (view.space.join_policy == JoinPolicy::Open && !is_guest(caller) && !view.space.is_archived)
    || is_instance_admin(caller);
  if is_visible {
    Ok(SpaceAccess { view })
  } else {
    Err(ApiError::space_not_found())
  }
}

/// Loads a conversation the caller may see.
///
/// Members always can. Public channels are visible to non-guest members of their space. Instance
/// admins see all channels (metadata only, never DMs).
pub async fn load_conversation(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
) -> Result<ConversationAccess, ApiError> {
  let view = state
    .database
    .get_conversation(id, caller.user.id)
    .await?
    .ok_or_else(ApiError::conversation_not_found)?;
  let maybe_space_role = match view.conversation.maybe_space_id {
    Some(space) => state.database.space_role(space, caller.user.id).await?,
    None => None,
  };

  let conversation = &view.conversation;
  let is_channel = conversation.kind == ConversationKind::Channel;
  let is_visible = view.maybe_membership.is_some()
    || (is_channel
      && conversation.visibility == ConversationVisibility::Public
      && !is_guest(caller)
      && maybe_space_role.is_some())
    || (is_channel && is_instance_admin(caller));
  if is_visible {
    Ok(ConversationAccess {
      view,
      maybe_space_role,
    })
  } else {
    Err(ApiError::conversation_not_found())
  }
}
