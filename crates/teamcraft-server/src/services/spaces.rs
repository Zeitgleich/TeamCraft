use serde_json::json;
use teamcraft_db::{ExpiryChange, NewSpace, NewSpaceMember, SpaceChanges, SpaceListing, SpaceView};
use teamcraft_proto::{
  AddSpaceMemberRequest, CategoryId, CategoryResponse, CreateCategoryRequest, CreateSpaceRequest,
  GatewayEvent, JoinPolicy, ListCategoriesResponse, ListSpaceMembersResponse, ListSpacesResponse, Permission,
  SpaceId, SpaceMemberResponse, SpaceResponse, SpaceRole, UnixMillis, UpdateCategoryRequest,
  UpdateSpaceMemberRequest, UpdateSpaceRequest, UserId, UserKind,
};
use time::OffsetDateTime;

use super::access::{is_guest, is_instance_admin, load_space, require, space_target};
use super::audit::{self, Target as AuditTarget};
use super::convert::{category_response, offset_date_time, space_response, unix_millis, user_summary};
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
    spaces: views
      .iter()
      .map(|view| space_for_caller(state, caller, view))
      .collect(),
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
  let limits = &state.settings.get().limits;
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
  let created = state
    .database
    .create_space(&NewSpace {
      id,
      name: &name,
      maybe_icon: request.maybe_icon.as_deref(),
      maybe_color: request.maybe_color.as_deref(),
      maybe_description: request.maybe_description.as_deref(),
      join_policy: request.join_policy,
      creator: caller.user.id,
      maybe_idempotency_key: request.maybe_idempotency_key,
    })
    .await?;
  // A retry of an earlier request: return that space, do not audit a second creation.
  if created.is_new {
    audit::record(
      state,
      Some(caller.user.id),
      "space_created",
      Some(AuditTarget::new("space", id)),
      &json!({}),
    )
    .await?;
  }
  let access = load_space(state, caller, created.id).await?;
  Ok(space_for_caller(state, caller, &access.view))
}

pub async fn get_space(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
) -> Result<SpaceResponse, ApiError> {
  Ok(space_for_caller(
    state,
    caller,
    &load_space(state, caller, id).await?.view,
  ))
}

/// The space as the caller sees it, with what they may do in it (UI hints; every request is checked
/// again).
fn space_for_caller(state: &AppState, caller: &AuthenticatedUser, view: &SpaceView) -> SpaceResponse {
  let target = space_target(Some(view.space.id), view.maybe_role.clone());
  let is_open_for_changes = !view.space.is_archived;
  let may = |permission| is_open_for_changes && require(state, caller, permission, &target).is_ok();
  SpaceResponse {
    can_add_members: may(Permission::ManageSpaceMembers),
    can_invite_guests: may(Permission::InviteGuest),
    can_manage_space: may(Permission::ManageSpaceProperties),
    can_create_channels: may(Permission::CreateChannel),
    ..space_response(view)
  }
}

pub async fn update_space(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: SpaceId,
  request: UpdateSpaceRequest,
) -> Result<SpaceResponse, ApiError> {
  let access = load_space(state, caller, id).await?;
  require(state, caller, Permission::ManageSpaceProperties, &access.target())?;
  let limits = &state.settings.get().limits;
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
  if let Some(brand_id) = request.maybe_brand_id.as_deref()
    && !brand_id.is_empty()
    && !state.brands.current().is_known_id(brand_id)
  {
    return Err(ApiError::InvalidRequest("unknown brand".into()));
  }
  state
    .database
    .update_space(
      id,
      &SpaceChanges {
        maybe_brand_id: request.maybe_brand_id.as_deref(),
        maybe_name: maybe_name.as_deref(),
        maybe_icon: request.maybe_icon.as_deref(),
        maybe_color: request.maybe_color.as_deref(),
        maybe_description: request.maybe_description.as_deref(),
        maybe_join_policy: request.maybe_join_policy,
        maybe_is_archived: request.maybe_is_archived,
      },
    )
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "space_updated",
    Some(AuditTarget::new("space", id)),
    &json!({}),
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
        expiry: ExpiryChange::Keep,
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
        user: user_summary(state, &member.user),
        role: member.role.clone(),
        maybe_expires_at: member.maybe_expires_at.map(unix_millis),
        maybe_sponsor_user_id: member.maybe_sponsor,
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
  // Adding is idempotent, but never changes the role of someone already in the space: that is
  // `update_space_member`, which also guards the last space admin.
  if let Some(current) = state.database.space_role(id, user.id).await? {
    if current == request.role {
      return member_response(state, id, &user, current).await;
    }
    return Err(ApiError::Conflict(
      "already a member of the space; change the role instead".into(),
    ));
  }
  let expiry = new_member_expiry(state, &request.role, request.maybe_expires_at)?;

  state
    .database
    .upsert_space_member(&NewSpaceMember {
      space: id,
      user: user.id,
      role: request.role.clone(),
      expiry,
      maybe_sponsor: Some(caller.user.id),
    })
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "space_member_added",
    Some(AuditTarget::new("space", id)),
    &json!({"user_id": user.id.to_string(), "role": request.role.as_str()}),
  )
  .await?;
  announce_member(state, id, user.id).await?;
  member_response(state, id, &user, request.role).await
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
  let expiry = changed_expiry(&request.role, request.maybe_expires_at, request.clears_expiry)?;
  state
    .database
    .upsert_space_member(&NewSpaceMember {
      space: id,
      user: user_id,
      role: request.role.clone(),
      expiry,
      // Whoever changes a member's terms does not become their sponsor.
      maybe_sponsor: None,
    })
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "space_member_role_changed",
    Some(AuditTarget::new("space", id)),
    &json!({"user_id": user_id.to_string(), "role": request.role.as_str()}),
  )
  .await?;
  notify_space(state, id, &GatewayEvent::SpaceUpdate { space_id: id }).await?;
  member_response(state, id, &user, request.role).await
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

  remove_everywhere(state, id, user_id).await?;
  audit::record(
    state,
    Some(caller.user.id),
    "space_member_removed",
    Some(AuditTarget::new("space", id)),
    &json!({"user_id": user_id.to_string()}),
  )
  .await?;
  Ok(())
}

/// Removes the user from the space and its channels and tells everybody concerned. Used by the
/// removal endpoint and by the guest expiry job.
pub async fn remove_everywhere(state: &AppState, id: SpaceId, user_id: UserId) -> Result<(), ApiError> {
  let remaining_before = state.database.list_space_members(id).await?;
  let left_channels = state.database.remove_space_member(id, user_id).await?;
  state.hub.publish(
    remaining_before
      .iter()
      .map(|member| member.user.id)
      .chain([user_id]),
    &GatewayEvent::SpaceMemberRemove {
      space_id: id,
      user_id,
    },
  );
  for channel in left_channels {
    super::calls::evict_from_call(state, channel, user_id).await;
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

/// The expiry of a newly added member: guests get the given one or the configured default.
fn new_member_expiry(
  state: &AppState,
  role: &SpaceRole,
  maybe_requested: Option<UnixMillis>,
) -> Result<ExpiryChange, ApiError> {
  if *role != SpaceRole::Guest {
    return match maybe_requested {
      Some(_) => Err(ApiError::InvalidRequest("only guests expire".into())),
      None => Ok(ExpiryChange::Keep),
    };
  }
  if let Some(millis) = maybe_requested {
    return Ok(ExpiryChange::Set(future_time(millis)?));
  }
  let days = super::workspace::effective_guest_expiry_days(state);
  Ok(if days == 0 {
    ExpiryChange::Keep
  } else {
    ExpiryChange::Set(OffsetDateTime::now_utc() + time::Duration::days(i64::from(days)))
  })
}

fn changed_expiry(
  role: &SpaceRole,
  maybe_requested: Option<UnixMillis>,
  clears: bool,
) -> Result<ExpiryChange, ApiError> {
  match (maybe_requested, clears) {
    (Some(_), true) => Err(ApiError::InvalidRequest(
      "set or clear the expiry, not both".into(),
    )),
    (None, false) => Ok(ExpiryChange::Keep),
    _ if *role != SpaceRole::Guest => Err(ApiError::InvalidRequest("only guests expire".into())),
    (Some(millis), false) => Ok(ExpiryChange::Set(future_time(millis)?)),
    (None, true) => Ok(ExpiryChange::Clear),
  }
}

fn future_time(millis: UnixMillis) -> Result<OffsetDateTime, ApiError> {
  let at = offset_date_time(millis).ok_or_else(|| ApiError::InvalidRequest("invalid expires_at".into()))?;
  if at <= OffsetDateTime::now_utc() {
    return Err(ApiError::InvalidRequest(
      "expires_at must be in the future".into(),
    ));
  }
  Ok(at)
}

async fn member_response(
  state: &AppState,
  space: SpaceId,
  user: &teamcraft_db::User,
  role: SpaceRole,
) -> Result<SpaceMemberResponse, ApiError> {
  let terms = state.database.space_member_terms(space, user.id).await?;
  Ok(SpaceMemberResponse {
    user: user_summary(state, user),
    role,
    maybe_expires_at: terms.and_then(|(expires, _)| expires).map(unix_millis),
    maybe_sponsor_user_id: terms.and_then(|(_, sponsor)| sponsor),
  })
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
    state.settings.get().limits.max_name_length,
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
    .map(|n| validate_name(n, "category name", state.settings.get().limits.max_name_length))
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
        user: user_summary(state, &member.user),
      },
    );
  }
  Ok(())
}

pub(super) async fn notify_space(
  state: &AppState,
  space: SpaceId,
  event: &GatewayEvent,
) -> Result<(), ApiError> {
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
