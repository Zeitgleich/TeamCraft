use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ProfileResponse, UpdateProfileRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::profiles;
use crate::state::AppState;

/// Replaces the caller's profile.
#[utoipa::path(put, path = "/api/v1/me/profile", tag = "profiles", summary = "Replaces the caller's profile.", request_body = UpdateProfileRequest, responses((status = 200, body = ProfileResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_my_profile_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<UpdateProfileRequest>,
) -> Result<Json<ProfileResponse>, ApiError> {
  let response = profiles::put_my_profile(&state, &caller, request).await?;
  Ok(Json(response))
}
