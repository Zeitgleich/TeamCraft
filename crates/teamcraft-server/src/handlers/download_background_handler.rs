use axum::Json;
use axum::extract::State;
use teamcraft_proto::{BackgroundDownloadResponse, BackgroundId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::backgrounds;
use crate::state::AppState;

/// A download URL for one of the caller's own encrypted call backgrounds.
#[utoipa::path(get, path = "/api/v1/me/backgrounds/{background_id}/download", tag = "profiles", summary = "A download URL for one of the caller's own encrypted call backgrounds.", params(("background_id" = BackgroundId, Path)), responses((status = 200, body = BackgroundDownloadResponse), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn download_background_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(background_id): ApiPath<BackgroundId>,
) -> Result<Json<BackgroundDownloadResponse>, ApiError> {
  Ok(Json(
    backgrounds::download_background(&state, &caller, background_id).await?,
  ))
}
