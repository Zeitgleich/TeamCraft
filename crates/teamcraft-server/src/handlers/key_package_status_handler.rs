use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DeviceId, ErrorBody, KeyPackageStatusResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::key_packages;
use crate::state::AppState;

/// Returns how many key packages the device has left.
#[utoipa::path(get, path = "/api/v1/devices/{device_id}/key-packages", tag = "e2ee", summary = "Returns how many key packages the device has left.", params(("device_id" = DeviceId, Path)), responses((status = 200, body = KeyPackageStatusResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn key_package_status_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(device_id): ApiPath<DeviceId>,
) -> Result<Json<KeyPackageStatusResponse>, ApiError> {
  let response = key_packages::key_package_status(&state, &caller, device_id).await?;
  Ok(Json(response))
}
