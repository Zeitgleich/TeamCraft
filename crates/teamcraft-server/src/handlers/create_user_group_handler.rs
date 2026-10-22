use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateUserGroupRequest, ErrorBody, UserGroupResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::user_groups;
use crate::state::AppState;

/// Creates a user group.
#[utoipa::path(post, path = "/api/v1/user-groups", tag = "user groups", summary = "Creates a user group.", request_body = CreateUserGroupRequest, responses((status = 201, body = UserGroupResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_user_group_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateUserGroupRequest>,
) -> Result<(StatusCode, Json<UserGroupResponse>), ApiError> {
  let response = user_groups::create_user_group(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
