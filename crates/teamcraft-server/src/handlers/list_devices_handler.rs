use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListDevicesResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::devices;
use crate::state::AppState;

/// Lists the caller's devices.
#[utoipa::path(get, path = "/api/v1/devices", tag = "devices", summary = "Lists the caller's devices.", responses((status = 200, body = ListDevicesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_devices_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListDevicesResponse>, ApiError> {
  let response = devices::list_devices(&state, &caller).await?;
  Ok(Json(response))
}
