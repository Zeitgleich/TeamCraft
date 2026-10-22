use axum::Json;
use axum::extract::State;
use teamcraft_proto::{DeviceId, ErrorBody, ListMlsMessagesQuery, ListMlsMessagesResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiPath, ApiQuery};
use crate::services::mls;
use crate::state::AppState;

/// Returns the welcomes waiting for one of the caller's devices.
#[utoipa::path(get, path = "/api/v1/devices/{device_id}/welcomes", tag = "e2ee", summary = "Returns the welcomes waiting for one of the caller's devices.", params(("device_id" = DeviceId, Path), ListMlsMessagesQuery), responses((status = 200, body = ListMlsMessagesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_welcomes_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(device_id): ApiPath<DeviceId>,
  ApiQuery(query): ApiQuery<ListMlsMessagesQuery>,
) -> Result<Json<ListMlsMessagesResponse>, ApiError> {
  let response = mls::list_welcomes(&state, &caller, device_id, query).await?;
  Ok(Json(response))
}
