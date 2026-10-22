use teamcraft_db::UserSearch;
use teamcraft_proto::{SearchUsersResponse, UserId, UserSummary};

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
) -> Result<SearchUsersResponse, ApiError> {
  let users = state
    .database
    .search_users(&UserSearch {
      viewer: caller.user.id,
      query: query.trim(),
      is_restricted_to_shared: is_guest(caller),
      limit: i64::from(state.settings.limits.max_directory_results),
    })
    .await?;
  Ok(SearchUsersResponse {
    users: users.iter().map(user_summary).collect(),
  })
}

/// One user; guests only see themselves and people they share a conversation with.
pub async fn get_user(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: UserId,
) -> Result<UserSummary, ApiError> {
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
  Ok(user_summary(&user))
}
