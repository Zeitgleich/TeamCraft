use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{DeviceId, ErrorBody, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin;
use crate::state::AppState;

/// Removes one device of a user.
#[utoipa::path(delete, path = "/api/v1/admin/users/{user_id}/devices/{device_id}", tag = "admin", summary = "Removes one device of a user.", params(("user_id" = UserId, Path), ("device_id" = DeviceId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_revoke_device_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath((user_id, device_id)): ApiPath<(UserId, DeviceId)>,
) -> Result<StatusCode, ApiError> {
  admin::revoke_user_device(&state, &caller, user_id, device_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
