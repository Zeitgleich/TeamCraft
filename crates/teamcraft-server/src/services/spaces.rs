use teamcraft_db::{NewSpace, NewSpaceMember, SpaceChanges, SpaceListing};
use teamcraft_proto::{
  AddSpaceMemberRequest, CategoryId, CategoryResponse, CreateCategoryRequest, CreateSpaceRequest,
  GatewayEvent, JoinPolicy, ListCategoriesResponse, ListSpaceMembersResponse, ListSpacesResponse, Permission,
  SpaceId, SpaceMemberResponse, SpaceResponse, SpaceRole, UpdateCategoryRequest, UpdateSpaceMemberRequest,
  UpdateSpaceRequest, UserId, UserKind,
};

use super::access::{is_guest, is_instance_admin, load_space, require};
use super::convert::{category_response, space_response, user_summary};
use super::{validate_name, validate_text};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

pub async fn list_spaces(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListSpacesResponse, ApiError> {
  let listing = if is_instance_admin(caller) {
    SpaceListing::All
  } else if is_guest(caller) {
    SpaceListing::Mine
  } else {
    SpaceListing::MineAndJoinable
  };
  let views = state
    .database
    .list_spaces_for_user(caller.user.id, listing)
    .await?;
  Ok(ListSpacesResponse {
    spaces: views.iter().map(space_response).collect(),
  })
}

pub async fn create_space(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateSpaceRequest,
) -> Result<SpaceResponse, ApiError> {
  require(
    state,
    caller,
    Permission::CreateSpace,
    &teamcraft_proto::Target::Instance,
  )?;
  let limits = &state.settings.limits;
  let name = validate_name(&request.name, "space name", limits.max_name_length)?;
  for text in [
    &request.maybe_icon,
    &request.maybe_color,
    &request.maybe_description,
  ]
  .into_iter()
  .flatten()
  {
    validate_text(text, "space field", limits.max_text_length)?;
  }
  let id: SpaceId = state.next_id()?;
  state
    .database
    .create_space(&NewSpace {
      id,
      name: &name,
      maybe_icon: request.maybe_icon.as_deref(),
      maybe_color: request.maybe_color.as_deref(),
      maybe_description: request.maybe_description.as_deref(),
      join_policy: request.join_policy,
      creator: caller.user.id,
    })
    .await?;
  let access = load_space(state, caller, id).await?;
  Ok(space_response(&access.view))
}

pub async fn get_space(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
) -> Result<SpaceResponse, ApiError> {
  Ok(space_response(&load_space(state, caller, id).await?.view))
}

pub async fn update_space(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
  request: UpdateSpaceRequest,
) -> Result<SpaceResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  require(state, caller, Permission::ManageSpaceProperties, &access.target())?;
  let limits = &state.settings.limits;
  let maybe_name = request
    .maybe_name
    .as_deref()
    .map(|n| validate_name(n, "space name", limits.max_name_length))
    .transpose()?;
  for text in [
    &request.maybe_icon,
    &request.maybe_color,
    &request.maybe_description,
  ]
  .into_iter()
  .flatten()
  {
    validate_text(text, "space field", limits.max_text_length)?;
  }
  state
    .database
    .update_space(
      id,
      &SpaceChanges {
        maybe_name: maybe_name.as_deref(),
        maybe_icon: request.maybe_icon.as_deref(),
        maybe_color: request.maybe_color.as_deref(),
        maybe_description: request.maybe_description.as_deref(),
        maybe_join_policy: request.maybe_join_policy,
        maybe_is_archived: request.maybe_is_archived,
      },
    )
    .await?;
  notify_space(state, id, &GatewayEvent::SpaceUpdate { space_id: id }).await?;
  get_space(state, caller, id).await
}

/// Joins an open space as a member. Guests are never self-joining; they are invited.
pub async fn join_space(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
) -> Result<SpaceResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  if access.view.maybe_role.is_none() {
    let space = &access.view.space;
    if is_guest(caller) || space.join_policy != JoinPolicy::Open || space.is_archived {
      return Err(ApiError::Forbidden(
        "this space can only be joined by invitation".into(),
      ));
    }
    state
      .database
      .upsert_space_member(&NewSpaceMember {
        space: id,
        user: caller.user.id,
        role: SpaceRole::Member,
        maybe_expires_at: None,
        maybe_sponsor: None,
      })
      .await?;
    announce_member(state, id, caller.user.id).await?;
  }
  get_space(state, caller, id).await
}

pub async fn list_space_members(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
) -> Result<ListSpaceMembersResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  // Guests have no directory; only non-guest members and admins see the roster.
  if is_guest(caller) || (access.view.maybe_role.is_none() && !is_instance_admin(caller)) {
    return Err(ApiError::Forbidden(
      "the member list is not available to you".into(),
    ));
  }
  let members = state.database.list_space_members(id).await?;
  Ok(ListSpaceMembersResponse {
    members: members
      .iter()
      .map(|member| SpaceMemberResponse {
        user: user_summary(&member.user),
        role: member.role.clone(),
      })
      .collect(),
  })
}

pub async fn add_space_member(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
  request: AddSpaceMemberRequest,
) -> Result<SpaceMemberResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  require_member_management(state, caller, &request.role, &access.target())?;
  let user = state
    .database
    .get_user(request.user_id)
    .await?
    .filter(|u| !u.is_deactivated);
  let Some(user) = user else {
    return Err(ApiError::user_not_found());
  };
  check_role_fits_user(&user.kind, &request.role)?;

  state
    .database
    .upsert_space_member(&NewSpaceMember {
      space: id,
      user: user.id,
      role: request.role.clone(),
      maybe_expires_at: None,
      maybe_sponsor: Some(caller.user.id),
    })
    .await?;
  announce_member(state, id, user.id).await?;
  Ok(SpaceMemberResponse {
    user: user_summary(&user),
    role: request.role,
  })
}

pub async fn update_space_member(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
  user_id: UserId,
  request: UpdateSpaceMemberRequest,
) -> Result<SpaceMemberResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  require_member_management(state, caller, &request.role, &access.target())?;
  let current = state
    .database
    .space_role(id, user_id)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  let user = state
    .database
    .get_user(user_id)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  check_role_fits_user(&user.kind, &request.role)?;
  if current == SpaceRole::SpaceAdmin && request.role != SpaceRole::SpaceAdmin {
    ensure_not_last_admin(state, id).await?;
  }
  state
    .database
    .upsert_space_member(&NewSpaceMember {
      space: id,
      user: user_id,
      role: request.role.clone(),
      maybe_expires_at: None,
      maybe_sponsor: Some(caller.user.id),
    })
    .await?;
  notify_space(state, id, &GatewayEvent::SpaceUpdate { space_id: id }).await?;
  Ok(SpaceMemberResponse {
    user: user_summary(&user),
    role: request.role,
  })
}

/// Removes a member (or leaves, if `user_id` is the caller). Also removes them from the space's channels.
pub async fn remove_space_member(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
  user_id: UserId,
) -> Result<(), ApiError> {
  let access = load_space(state, caller, id).await?;
  if user_id != caller.user.id {
    require(state, caller, Permission::ManageSpaceMembers, &access.target())?;
  }
  let current = state
    .database
    .space_role(id, user_id)
    .await?
    .ok_or_else(ApiError::user_not_found)?;
  if current == SpaceRole::SpaceAdmin {
    ensure_not_last_admin(state, id).await?;
  }

  let remaining_before = state.database.list_space_members(id).await?;
  let left_channels = state.database.remove_space_member(id, user_id).await?;
  state.hub.publish(
    remaining_before.iter().map(|member| member.user.id),
    &GatewayEvent::SpaceMemberRemove {
      space_id: id,
      user_id,
    },
  );
  for channel in left_channels {
    let members = state.database.conversation_member_ids(channel).await?;
    state.hub.publish(
      members.into_iter().chain([user_id]),
      &GatewayEvent::MembershipRemove {
        conversation_id: channel,
        user_id,
      },
    );
  }
  Ok(())
}

pub async fn list_categories(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
) -> Result<ListCategoriesResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  if access.view.maybe_role.is_none() && !is_instance_admin(caller) && is_guest(caller) {
    return Err(ApiError::space_not_found());
  }
  let categories = state.database.list_categories(id).await?;
  Ok(ListCategoriesResponse {
    categories: categories.iter().map(category_response).collect(),
  })
}

pub async fn create_category(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
  request: CreateCategoryRequest,
) -> Result<CategoryResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  require(state, caller, Permission::ManageSpaceProperties, &access.target())?;
  let name = validate_name(
    &request.name,
    "category name",
    state.settings.limits.max_name_length,
  )?;
  let category = state
    .database
    .create_category(state.next_id()?, id, &name, request.maybe_position)
    .await?;
  notify_space(state, id, &GatewayEvent::SpaceUpdate { space_id: id }).await?;
  Ok(category_response(&category))
}

pub async fn update_category(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  category: CategoryId,
  request: UpdateCategoryRequest,
) -> Result<CategoryResponse, ApiError> {
  let access = load_space(state, caller, space).await?;
  require(state, caller, Permission::ManageSpaceProperties, &access.target())?;
  load_category(state, space, category).await?;
  let maybe_name = request
    .maybe_name
    .as_deref()
    .map(|n| validate_name(n, "category name", state.settings.limits.max_name_length))
    .transpose()?;
  state
    .database
    .update_category(category, maybe_name.as_deref(), request.maybe_position)
    .await?;
  notify_space(state, space, &GatewayEvent::SpaceUpdate { space_id: space }).await?;
  Ok(category_response(&load_category(state, space, category).await?))
}

pub async fn delete_category(
  state: &AppState,
  caller: &AuthenticatedUser,
  space: SpaceId,
  category: CategoryId,
) -> Result<(), ApiError> {
  let access = load_space(state, caller, space).await?;
  require(state, caller, Permission::ManageSpaceProperties, &access.target())?;
  load_category(state, space, category).await?;
  state.database.delete_category(category).await?;
  notify_space(state, space, &GatewayEvent::SpaceUpdate { space_id: space }).await?;
  Ok(())
}

async fn load_category(
  state: &AppState,
  space: SpaceId,
  category: CategoryId,
) -> Result<teamcraft_db::Category, ApiError> {
  state
    .database
    .get_category(category)
    .await?
    .filter(|c| c.space_id == space)
    .ok_or_else(ApiError::category_not_found)
}

/// Adding a guest needs `invite_guest`, anyone else `manage_space_members`.
pub(super) fn require_member_management(
  state: &AppState,
  caller: &AuthenticatedUser,
  role: &SpaceRole,
  target: &teamcraft_proto::Target,
) -> Result<(), ApiError> {
  let permission = if *role == SpaceRole::Guest {
    Permission::InviteGuest
  } else {
    Permission::ManageSpaceMembers
  };
  require(state, caller, permission, target)
}

/// Guest accounts only ever get the guest role (they can't be promoted by space admins).
fn check_role_fits_user(kind: &UserKind, role: &SpaceRole) -> Result<(), ApiError> {
  if *kind == UserKind::Guest && *role != SpaceRole::Guest {
    return Err(ApiError::InvalidRequest(
      "guest accounts can only have the guest role".into(),
    ));
  }
  if matches!(role, SpaceRole::Unknown(_)) {
    return Err(ApiError::InvalidRequest("unknown space role".into()));
  }
  Ok(())
}

async fn ensure_not_last_admin(state: &AppState, space: SpaceId) -> Result<(), ApiError> {
  if state.database.count_space_admins(space).await? <= 1 {
    return Err(ApiError::Conflict(
      "a space needs at least one space admin".into(),
    ));
  }
  Ok(())
}

async fn announce_member(state: &AppState, space: SpaceId, user: UserId) -> Result<(), ApiError> {
  let members = state.database.list_space_members(space).await?;
  if let Some(member) = members.iter().find(|m| m.user.id == user) {
    state.hub.publish(
      members.iter().map(|m| m.user.id),
      &GatewayEvent::SpaceMemberAdd {
        space_id: space,
        user: user_summary(&member.user),
      },
    );
  }
  Ok(())
}

async fn notify_space(state: &AppState, space: SpaceId, event: &GatewayEvent) -> Result<(), ApiError> {
  let members = state.database.list_space_members(space).await?;
  state.hub.publish(members.iter().map(|m| m.user.id), event);
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn guest_accounts_only_fit_the_guest_role() {
    assert!(check_role_fits_user(&UserKind::Guest, &SpaceRole::Guest).is_ok());
    assert!(check_role_fits_user(&UserKind::Guest, &SpaceRole::Member).is_err());
    assert!(check_role_fits_user(&UserKind::Guest, &SpaceRole::SpaceAdmin).is_err());
    assert!(check_role_fits_user(&UserKind::Member, &SpaceRole::SpaceAdmin).is_ok());
    assert!(check_role_fits_user(&UserKind::Member, &SpaceRole::Unknown("x".into())).is_err());
  }
}
