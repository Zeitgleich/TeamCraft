use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::ErrorBody;

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::local_accounts;
use crate::state::AppState;

/// Ends the caller's local session.
#[utoipa::path(post, path = "/api/v1/auth/local/logout", tag = "auth", summary = "Ends the caller's local session.", responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn local_logout_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<StatusCode, ApiError> {
  local_accounts::logout(&state, &caller).await?;
  Ok(StatusCode::NO_CONTENT)
}
