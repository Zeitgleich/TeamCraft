use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{BackgroundId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::backgrounds;
use crate::state::AppState;

/// Confirms a background upload after checking the stored size.
#[utoipa::path(post, path = "/api/v1/me/backgrounds/{background_id}/complete", tag = "profiles", summary = "Confirms a background upload after checking the stored size.", params(("background_id" = BackgroundId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn complete_background_upload_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(background_id): ApiPath<BackgroundId>,
) -> Result<StatusCode, ApiError> {
  backgrounds::complete_background_upload(&state, &caller, background_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
