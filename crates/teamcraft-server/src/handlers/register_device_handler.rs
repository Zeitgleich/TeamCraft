use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{DeviceResponse, ErrorBody, RegisterDeviceRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::devices;
use crate::state::AppState;

/// Registers a device with its public keys.
#[utoipa::path(post, path = "/api/v1/devices", tag = "devices", summary = "Registers a device with its public keys.", request_body = RegisterDeviceRequest, responses((status = 201, body = DeviceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn register_device_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<RegisterDeviceRequest>,
) -> Result<(StatusCode, Json<DeviceResponse>), ApiError> {
  let response = devices::register_device(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
