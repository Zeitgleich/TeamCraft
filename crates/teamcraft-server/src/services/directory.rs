use teamcraft_db::UserSearch;
use teamcraft_proto::{ListOrganizationsResponse, SearchUsersResponse, UserId, UserSummary};

use super::access::is_guest;
use super::convert::user_summary;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Directory search. Guests only find people they share a conversation with (PLAN.md §12).
pub async fn search_users(
  state: &AppState,
  caller: &AuthenticatedUser,
  query: &str,
  maybe_organization: Option<&str>,
) -> Result<SearchUsersResponse, ApiError> {
  let users = state
    .database
    .search_users(&UserSearch {
      viewer: caller.user.id,
      query: query.trim(),
      is_restricted_to_shared: is_guest(caller),
      maybe_organization,
      limit: i64::from(state.settings.limits.max_directory_results),
    })
    .await?;
  let mut summaries: Vec<UserSummary> = users.iter().map(|user| user_summary(state, user)).collect();
  add_emails(state, caller, &mut summaries).await?;
  Ok(SearchUsersResponse { users: summaries })
}

/// The organizations for the directory filter; guests have no directory and get none.
pub async fn list_organizations(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<ListOrganizationsResponse, ApiError> {
  let organizations = if is_guest(caller) {
    Vec::new()
  } else {
    state.database.list_organizations().await?
  };
  Ok(ListOrganizationsResponse { organizations })
}

/// One user; guests only see themselves and people they share a conversation with.
pub async fn get_user(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserId,
) -> Result<UserSummary, ApiError> {
  let mut summary = vec![user_summary(state, &visible_user(state, caller, id).await?)];
  add_emails(state, caller, &mut summary).await?;
  summary
    .pop()
    .ok_or_else(|| ApiError::Internal("summary missing".into()))
}

/// Adds e-mail addresses if the directory policy shows them. Guests never see any.
async fn add_emails(
  state: &AppState,
  caller: &AuthenticatedUser,
  summaries: &mut [UserSummary],
) -> Result<(), ApiError> {
  if !state.policy.get().directory.shows_email || is_guest(caller) {
    return Ok(());
  }
  let ids: Vec<UserId> = summaries.iter().map(|summary| summary.id).collect();
  let emails = state.database.get_user_emails(&ids).await?;
  for summary in summaries {
    summary.maybe_email = emails.get(&summary.id).cloned();
  }
  Ok(())
}

/// The active user the caller may know about, or `user_not_found`.
pub async fn visible_user(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserId,
) -> Result<teamcraft_db::User, ApiError> {
  let user = state
    .database
    .get_user(id)
    .await?
    .filter(|user| !user.is_deactivated);
  let Some(user) = user else {
    return Err(ApiError::user_not_found());
  };
  if is_guest(caller)
    && user.id != caller.user.id
    && !state
      .database
      .users_share_conversation(caller.user.id, id)
      .await?
  {
    return Err(ApiError::user_not_found());
  }
  Ok(user)
}
