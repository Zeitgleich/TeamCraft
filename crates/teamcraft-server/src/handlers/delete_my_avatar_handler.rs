use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::ErrorBody;

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::profiles;
use crate::state::AppState;

/// Removes the caller's avatar.
#[utoipa::path(delete, path = "/api/v1/me/avatar", tag = "profiles", summary = "Removes the caller's avatar.", responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_my_avatar_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<StatusCode, ApiError> {
  profiles::delete_my_avatar(&state, &caller).await?;
  Ok(StatusCode::NO_CONTENT)
}
