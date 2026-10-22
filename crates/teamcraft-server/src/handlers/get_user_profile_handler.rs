use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ProfileResponse, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::profiles;
use crate::state::AppState;

/// Returns the profile of a user the caller may see.
#[utoipa::path(get, path = "/api/v1/users/{user_id}/profile", tag = "profiles", summary = "Returns the profile of a user the caller may see.", params(("user_id" = UserId, Path)), responses((status = 200, body = ProfileResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_user_profile_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<ProfileResponse>, ApiError> {
  let response = profiles::get_profile(&state, &caller, user_id).await?;
  Ok(Json(response))
}
