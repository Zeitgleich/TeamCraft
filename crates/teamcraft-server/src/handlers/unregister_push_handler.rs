use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{DeviceId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::devices;
use crate::state::AppState;

/// Removes the device's push registration.
#[utoipa::path(delete, path = "/api/v1/devices/{device_id}/push", tag = "devices", summary = "Removes the device's push registration.", params(("device_id" = DeviceId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn unregister_push_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(device_id): ApiPath<DeviceId>,
) -> Result<StatusCode, ApiError> {
  devices::unregister_push(&state, &caller, device_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
