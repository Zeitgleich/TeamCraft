use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::ErrorBody;

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::device_links;
use crate::state::AppState;

/// Withdraws a link request.
#[utoipa::path(delete, path = "/api/v1/device-links/{request_id}", tag = "devices", summary = "Withdraws a link request.", params(("request_id" = String, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn withdraw_device_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<String>,
) -> Result<StatusCode, ApiError> {
  device_links::withdraw(&state, &caller, &request_id)?;
  Ok(StatusCode::NO_CONTENT)
}
