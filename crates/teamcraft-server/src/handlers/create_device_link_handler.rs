use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CreateDeviceLinkRequest, DeviceLinkRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::device_links;
use crate::state::AppState;

/// Announces a waiting new device to the caller's other devices.
#[utoipa::path(post, path = "/api/v1/device-links", tag = "devices", summary = "Announces a waiting new device to the caller's other devices.", request_body = CreateDeviceLinkRequest, responses((status = 200, body = DeviceLinkRequest), (status = 409, body = ErrorBody), (status = 429, body = ErrorBody), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_device_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  Json(request): Json<CreateDeviceLinkRequest>,
) -> Result<Json<DeviceLinkRequest>, ApiError> {
  Ok(Json(device_links::announce(&state, &caller, request).await?))
}
