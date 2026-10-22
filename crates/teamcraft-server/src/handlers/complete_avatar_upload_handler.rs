use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ProfileResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::profiles;
use crate::state::AppState;

/// Checks the uploaded image and makes it the avatar.
#[utoipa::path(post, path = "/api/v1/me/avatar/complete", tag = "profiles", summary = "Checks the uploaded image and makes it the avatar.", responses((status = 200, body = ProfileResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn complete_avatar_upload_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ProfileResponse>, ApiError> {
  let response = profiles::complete_avatar_upload(&state, &caller).await?;
  Ok(Json(response))
}
