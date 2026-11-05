use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::bots;
use crate::state::AppState;

/// Takes a bot's picture away.
#[utoipa::path(delete, path = "/api/v1/admin/bots/{bot_id}/avatar", tag = "admin", summary = "Takes a bot's picture away.", params(("bot_id" = UserId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_delete_bot_avatar_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(bot_id): ApiPath<UserId>,
) -> Result<StatusCode, ApiError> {
  bots::delete_bot_avatar(&state, &caller, bot_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
