use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DeviceListResponse, ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::device_lists;
use crate::state::AppState;

/// Returns the newest signed device list of a user.
#[utoipa::path(get, path = "/api/v1/users/{user_id}/device-list", tag = "e2ee", summary = "Returns the newest signed device list of a user.", params(("user_id" = UserId, Path)), responses((status = 200, body = DeviceListResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_user_device_list_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<DeviceListResponse>, ApiError> {
  let response = device_lists::get_device_list(&state, &caller, user_id).await?;
  Ok(Json(response))
}
