use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DeviceId, DeviceResponse, ErrorBody, UpdateDeviceRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::devices;
use crate::state::AppState;

/// Renames one of the caller's devices.
#[utoipa::path(patch, path = "/api/v1/devices/{device_id}", tag = "devices", summary = "Renames one of the caller's devices.", request_body = UpdateDeviceRequest, params(("device_id" = DeviceId, Path)), responses((status = 200, body = DeviceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_device_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(device_id): ApiPath<DeviceId>,
  ApiJson(request): ApiJson<UpdateDeviceRequest>,
) -> Result<Json<DeviceResponse>, ApiError> {
  let response = devices::update_device(&state, &caller, device_id, request).await?;
  Ok(Json(response))
}
