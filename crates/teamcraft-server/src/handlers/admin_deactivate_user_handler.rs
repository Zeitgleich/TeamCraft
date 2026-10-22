use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{DeactivateUserRequest, ErrorBody, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::admin;
use crate::state::AppState;

/// Deactivates a user: sessions end, devices are removed, groups are left.
#[utoipa::path(post, path = "/api/v1/admin/users/{user_id}/deactivate", tag = "admin", summary = "Deactivates a user: sessions end, devices are removed, groups are left.", request_body = DeactivateUserRequest, params(("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_deactivate_user_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(user_id): ApiPath<UserId>,
  ApiJson(request): ApiJson<DeactivateUserRequest>,
) -> Result<StatusCode, ApiError> {
  admin::deactivate_user_as_admin(&state, &caller, user_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
