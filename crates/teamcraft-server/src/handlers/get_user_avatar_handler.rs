use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AvatarResponse, ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::profiles;
use crate::state::AppState;

/// Returns a presigned download URL of a user's avatar.
#[utoipa::path(get, path = "/api/v1/users/{user_id}/avatar", tag = "profiles", summary = "Returns a presigned download URL of a user's avatar.", params(("user_id" = UserId, Path)), responses((status = 200, body = AvatarResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_user_avatar_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<AvatarResponse>, ApiError> {
  let response = profiles::get_avatar(&state, &caller, user_id).await?;
  Ok(Json(response))
}
