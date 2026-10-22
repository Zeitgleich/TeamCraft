use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{DeviceId, ErrorBody, RegisterPushRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::devices;
use crate::state::AppState;

/// Registers the device's push channel (provider, token, push key).
#[utoipa::path(put, path = "/api/v1/devices/{device_id}/push", tag = "devices", summary = "Registers the device's push channel (provider, token, push key).", request_body = RegisterPushRequest, params(("device_id" = DeviceId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn register_push_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(device_id): ApiPath<DeviceId>,
  ApiJson(request): ApiJson<RegisterPushRequest>,
) -> Result<StatusCode, ApiError> {
  devices::register_push(&state, &caller, device_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
