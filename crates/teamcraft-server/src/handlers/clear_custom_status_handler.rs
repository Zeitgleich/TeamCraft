use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::ErrorBody;

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::presence;
use crate::state::AppState;

/// Clears the caller's custom status.
#[utoipa::path(delete, path = "/api/v1/me/status", tag = "presence", summary = "Clears the caller's custom status.", responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn clear_custom_status_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<StatusCode, ApiError> {
  presence::clear_custom_status(&state, &caller).await?;
  Ok(StatusCode::NO_CONTENT)
}
