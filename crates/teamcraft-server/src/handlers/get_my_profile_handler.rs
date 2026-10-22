use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ProfileResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::profiles;
use crate::state::AppState;

/// Returns the caller's profile.
#[utoipa::path(get, path = "/api/v1/me/profile", tag = "profiles", summary = "Returns the caller's profile.", responses((status = 200, body = ProfileResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_my_profile_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ProfileResponse>, ApiError> {
  let response = profiles::get_my_profile(&state, &caller).await?;
  Ok(Json(response))
}
