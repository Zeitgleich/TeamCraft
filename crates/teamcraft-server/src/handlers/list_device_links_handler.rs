use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListDeviceLinksResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::device_links;
use crate::state::AppState;

/// Lists the open link requests of the caller.
#[utoipa::path(get, path = "/api/v1/device-links", tag = "devices", summary = "Lists the open link requests of the caller.", responses((status = 200, body = ListDeviceLinksResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_device_links_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListDeviceLinksResponse>, ApiError> {
  Ok(Json(device_links::list(&state, &caller)))
}
