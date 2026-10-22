use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, UserGroupDetailResponse, UserGroupId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::user_groups;
use crate::state::AppState;

/// Returns a user group with its members.
#[utoipa::path(get, path = "/api/v1/user-groups/{group_id}", tag = "user groups", summary = "Returns a user group with its members.", params(("group_id" = UserGroupId, Path)), responses((status = 200, body = UserGroupDetailResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_user_group_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(group_id): ApiPath<UserGroupId>,
) -> Result<Json<UserGroupDetailResponse>, ApiError> {
  let response = user_groups::get_user_group(&state, &caller, group_id).await?;
  Ok(Json(response))
}
