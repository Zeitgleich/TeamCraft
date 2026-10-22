use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, UserId, UserSummary};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::directory;
use crate::state::AppState;

/// Looks up one user.
#[utoipa::path(get, path = "/api/v1/users/{user_id}", tag = "directory", summary = "Looks up one user.", params(("user_id" = UserId, Path)), responses((status = 200, body = UserSummary), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_user_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<UserSummary>, ApiError> {
  let response = directory::get_user(&state, &caller, user_id).await?;
  Ok(Json(response))
}
