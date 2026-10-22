use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{BackgroundId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::backgrounds;
use crate::state::AppState;

/// Deletes one of the caller's own call backgrounds and its stored object.
#[utoipa::path(delete, path = "/api/v1/me/backgrounds/{background_id}", tag = "profiles", summary = "Deletes one of the caller's own call backgrounds and its stored object.", params(("background_id" = BackgroundId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_background_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(background_id): ApiPath<BackgroundId>,
) -> Result<StatusCode, ApiError> {
  backgrounds::delete_background(&state, &caller, background_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
