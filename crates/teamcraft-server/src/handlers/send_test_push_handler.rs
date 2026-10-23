use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DeviceId, ErrorBody, TestPushResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::push_diagnostics;
use crate::state::AppState;

/// Sends a sealed test wake-up to one of the caller's devices (notification diagnostics).
#[utoipa::path(post, path = "/api/v1/devices/{device_id}/push/test", tag = "devices", summary = "Sends a sealed test wake-up to one of the caller's devices (notification diagnostics).", params(("device_id" = DeviceId, Path)), responses((status = 200, body = TestPushResponse), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)), security(("bearer" = [])))]
pub async fn send_test_push_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(device_id): ApiPath<DeviceId>,
) -> Result<Json<TestPushResponse>, ApiError> {
  let response = push_diagnostics::send_test_push(&state, &caller, device_id).await?;
  Ok(Json(response))
}
