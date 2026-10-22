//! User groups (`@devteam`, PLAN.md §4): named sets of people, managed by admins or synced from
//! the identity provider's groups. The server only keeps the sets; mentions are end-to-end
//! encrypted and expanded by clients.

use serde_json::json;
use teamcraft_db::{GroupCreateOutcome, NewUserGroup, UserGroup};
use teamcraft_proto::{
  CreateUserGroupRequest, GatewayEvent, ListUserGroupsResponse, Permission, Target, UpdateUserGroupRequest,
  UserGroupDetailResponse, UserGroupId, UserGroupResponse, UserId, UserKind,
};

use super::access::{is_guest, require};
use super::audit::{self, Target as AuditTarget};
use super::convert::user_summary;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

const MIN_HANDLE_CHARS: usize = 2;
const MAX_HANDLE_CHARS: usize = 32;
/// Mentions with a meaning of their own.
const RESERVED_HANDLES: [&str; 6] = ["all", "channel", "here", "everyone", "everybody", "admins"];

pub async fn list_user_groups(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListUserGroupsResponse, ApiError> {
  require_not_guest(caller)?;
  let groups = state.database.list_user_groups().await?;
  Ok(ListUserGroupsResponse {
    groups: groups.iter().map(group_response).collect(),
  })
}

pub async fn get_user_group(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserGroupId,
) -> Result<UserGroupDetailResponse, ApiError> {
  require_not_guest(caller)?;
  let group = load(state, id).await?;
  let member_ids = state.database.list_user_group_member_ids(id).await?;
  let members = state.database.get_users(&member_ids).await?;
  Ok(UserGroupDetailResponse {
    group: group_response(&group),
    members: members
      .iter()
      .filter(|user| !user.is_deactivated)
      .map(|user| user_summary(state, user))
      .collect(),
  })
}

pub async fn create_user_group(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateUserGroupRequest,
) -> Result<UserGroupResponse, ApiError> {
  require(state, caller, Permission::CreateUserGroup, &Target::Instance)?;
  let handle = request.handle.trim().trim_start_matches('@').to_lowercase();
  check_handle(&handle)?;
  let name = clean_name(state, &request.name)?;
  let maybe_description = clean_description(state, request.maybe_description)?;
  let id: UserGroupId = state.next_id()?;
  let outcome = state
    .database
    .create_user_group(&NewUserGroup {
      id,
      handle: &handle,
      name: &name,
      maybe_description: maybe_description.as_deref(),
      maybe_external_id: None,
      maybe_created_by: Some(caller.user.id),
    })
    .await?;
  if outcome == GroupCreateOutcome::HandleTaken {
    return Err(ApiError::Conflict(format!(
      "the handle {handle} is used by a group or a person"
    )));
  }
  audit::record(
    state,
    Some(caller.user.id),
    "user_group_created",
    Some(AuditTarget::new("user_group", id)),
    &json!({"handle": handle}),
  )
  .await?;
  announce(state, id);
  Ok(group_response(&load(state, id).await?))
}

pub async fn update_user_group(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserGroupId,
  request: UpdateUserGroupRequest,
) -> Result<UserGroupResponse, ApiError> {
  require(state, caller, Permission::CreateUserGroup, &Target::Instance)?;
  let group = load(state, id).await?;
  require_local(&group)?;
  let maybe_name = request
    .maybe_name
    .map(|name| clean_name(state, &name))
    .transpose()?;
  let maybe_description = clean_description(state, request.maybe_description)?;
  state
    .database
    .update_user_group(
      id,
      maybe_name.as_deref(),
      maybe_description.as_deref(),
      request.clears_description,
    )
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "user_group_updated",
    Some(AuditTarget::new("user_group", id)),
    &json!({"handle": group.handle}),
  )
  .await?;
  announce(state, id);
  Ok(group_response(&load(state, id).await?))
}

pub async fn delete_user_group(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserGroupId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::CreateUserGroup, &Target::Instance)?;
  let group = load(state, id).await?;
  require_local(&group)?;
  state.database.delete_user_group(id).await?;
  audit::record(
    state,
    Some(caller.user.id),
    "user_group_deleted",
    Some(AuditTarget::new("user_group", id)),
    &json!({"handle": group.handle}),
  )
  .await?;
  announce(state, id);
  Ok(())
}

pub async fn add_user_group_member(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserGroupId,
  user: UserId,
) -> Result<(), ApiError> {
  require(
    state,
    caller,
    Permission::ManageUserGroupMembers,
    &Target::Instance,
  )?;
  let group = load(state, id).await?;
  require_local(&group)?;
  let member = state
    .database
    .get_user(user)
    .await?
    .filter(|user| !user.is_deactivated)
    .ok_or_else(ApiError::user_not_found)?;
  if member.kind != UserKind::Member {
    return Err(ApiError::InvalidRequest(
      "only members can belong to user groups".into(),
    ));
  }
  if group.member_count as usize >= state.settings.limits.max_user_group_members {
    return Err(ApiError::Conflict("the group is full".into()));
  }
  if state.database.add_user_group_member(id, user).await? {
    audit::record(
      state,
      Some(caller.user.id),
      "user_group_member_added",
      Some(AuditTarget::new("user_group", id)),
      &json!({"user_id": user.to_string()}),
    )
    .await?;
    announce(state, id);
  }
  Ok(())
}

pub async fn remove_user_group_member(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserGroupId,
  user: UserId,
) -> Result<(), ApiError> {
  require(
    state,
    caller,
    Permission::ManageUserGroupMembers,
    &Target::Instance,
  )?;
  let group = load(state, id).await?;
  require_local(&group)?;
  if state.database.remove_user_group_member(id, user).await? {
    audit::record(
      state,
      Some(caller.user.id),
      "user_group_member_removed",
      Some(AuditTarget::new("user_group", id)),
      &json!({"user_id": user.to_string()}),
    )
    .await?;
    announce(state, id);
  }
  Ok(())
}

/// Tells everybody online that the group changed.
pub fn announce(state: &AppState, id: UserGroupId) {
  state
    .hub
    .publish_all(&GatewayEvent::UserGroupUpdate { group_id: id });
}

/// Lower case letters, digits, `-` and `_`; starts with a letter or digit.
pub fn check_handle(handle: &str) -> Result<(), ApiError> {
  let length = handle.chars().count();
  let is_valid = (MIN_HANDLE_CHARS..=MAX_HANDLE_CHARS).contains(&length)
    && handle.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
    && handle
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'));
  if !is_valid {
    return Err(ApiError::InvalidRequest(format!(
      "a handle has {MIN_HANDLE_CHARS} to {MAX_HANDLE_CHARS} lower case letters, digits, - or _"
    )));
  }
  if RESERVED_HANDLES.contains(&handle) {
    return Err(ApiError::InvalidRequest(format!("{handle} is reserved")));
  }
  Ok(())
}

/// A handle for a group name of the identity provider, if the name can become one.
pub fn handle_from_name(name: &str) -> Option<String> {
  let mut handle = String::new();
  for c in name.trim().trim_start_matches('/').chars() {
    if c.is_ascii_alphanumeric() || (c == '_' && !handle.is_empty()) {
      handle.push(c.to_ascii_lowercase());
    } else if matches!(c, '-' | ' ' | '/' | '.') && !handle.ends_with('-') && !handle.is_empty() {
      handle.push('-');
    }
  }
  let handle = handle.trim_end_matches('-').to_owned();
  check_handle(&handle).ok().map(|()| handle)
}

async fn load(state: &AppState, id: UserGroupId) -> Result<UserGroup, ApiError> {
  state
    .database
    .get_user_group(id)
    .await?
    .ok_or_else(ApiError::user_group_not_found)
}

/// Groups of the identity provider are edited there.
fn require_local(group: &UserGroup) -> Result<(), ApiError> {
  if group.maybe_external_id.is_some() {
    return Err(ApiError::Conflict(
      "this group is synced from the identity provider; change it there".into(),
    ));
  }
  Ok(())
}

fn require_not_guest(caller: &AuthenticatedUser) -> Result<(), ApiError> {
  if is_guest(caller) {
    return Err(ApiError::Forbidden("guests do not see user groups".into()));
  }
  Ok(())
}

fn clean_name(state: &AppState, name: &str) -> Result<String, ApiError> {
  let name = name.trim();
  if name.is_empty() || name.chars().count() > state.settings.limits.max_name_length {
    return Err(ApiError::InvalidRequest("invalid name".into()));
  }
  Ok(name.to_owned())
}

fn clean_description(
  state: &AppState,
  maybe_description: Option<String>,
) -> Result<Option<String>, ApiError> {
  let Some(text) = maybe_description else {
    return Ok(None);
  };
  let text = text.trim();
  if text.is_empty() {
    return Ok(None);
  }
  if text.chars().count() > state.settings.limits.max_text_length {
    return Err(ApiError::InvalidRequest("the description is too long".into()));
  }
  Ok(Some(text.to_owned()))
}

fn group_response(group: &UserGroup) -> UserGroupResponse {
  UserGroupResponse {
    id: group.id,
    handle: group.handle.clone(),
    name: group.name.clone(),
    maybe_description: group.maybe_description.clone(),
    is_synced: group.maybe_external_id.is_some(),
    member_count: group.member_count,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn handles_are_lower_case_words() {
    for good in ["devteam", "ab", "team-1", "a_b", "9lives"] {
      assert!(check_handle(good).is_ok(), "{good}");
    }
    for bad in [
      "",
      "a",
      "Dev",
      "dev team",
      "-dev",
      "_dev",
      "dev!",
      "all",
      "channel",
      "x".repeat(33).as_str(),
    ] {
      assert!(check_handle(bad).is_err(), "{bad}");
    }
  }

  #[test]
  fn provider_group_names_become_handles_when_they_can() {
    assert_eq!(handle_from_name("Dev Team").as_deref(), Some("dev-team"));
    assert_eq!(handle_from_name("/project-a").as_deref(), Some("project-a"));
    assert_eq!(handle_from_name("Ops_2024").as_deref(), Some("ops_2024"));
    assert_eq!(handle_from_name("Äpfel & Birnen").as_deref(), Some("pfel-birnen"));
    assert_eq!(handle_from_name("x"), None);
    assert_eq!(handle_from_name("!!!"), None);
    assert_eq!(handle_from_name("All"), None);
  }
}
