use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, UserGroupId, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::user_groups;
use crate::state::AppState;

/// Removes a member from a user group.
#[utoipa::path(delete, path = "/api/v1/user-groups/{group_id}/members/{user_id}", tag = "user groups", summary = "Removes a member from a user group.", params(("group_id" = UserGroupId, Path), ("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn remove_user_group_member_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((group_id, user_id)): ApiPath<(UserGroupId, UserId)>,
) -> Result<StatusCode, ApiError> {
  user_groups::remove_user_group_member(&state, &caller, group_id, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
