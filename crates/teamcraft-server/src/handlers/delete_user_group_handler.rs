use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, UserGroupId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::user_groups;
use crate::state::AppState;

/// Deletes a user group.
#[utoipa::path(delete, path = "/api/v1/user-groups/{group_id}", tag = "user groups", summary = "Deletes a user group.", params(("group_id" = UserGroupId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_user_group_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(group_id): ApiPath<UserGroupId>,
) -> Result<StatusCode, ApiError> {
  user_groups::delete_user_group(&state, &caller, group_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
