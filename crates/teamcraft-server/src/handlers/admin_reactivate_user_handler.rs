use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin;
use crate::state::AppState;

/// Lifts a deactivation.
#[utoipa::path(post, path = "/api/v1/admin/users/{user_id}/reactivate", tag = "admin", summary = "Lifts a deactivation.", params(("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_reactivate_user_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<StatusCode, ApiError> {
  admin::reactivate_user(&state, &caller, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
