use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, UpdateUserGroupRequest, UserGroupId, UserGroupResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::user_groups;
use crate::state::AppState;

/// Renames a user group or changes its description.
#[utoipa::path(patch, path = "/api/v1/user-groups/{group_id}", tag = "user groups", summary = "Renames a user group or changes its description.", request_body = UpdateUserGroupRequest, params(("group_id" = UserGroupId, Path)), responses((status = 200, body = UserGroupResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_user_group_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(group_id): ApiPath<UserGroupId>,
  ApiJson(request): ApiJson<UpdateUserGroupRequest>,
) -> Result<Json<UserGroupResponse>, ApiError> {
  let response = user_groups::update_user_group(&state, &caller, group_id, request).await?;
  Ok(Json(response))
}
