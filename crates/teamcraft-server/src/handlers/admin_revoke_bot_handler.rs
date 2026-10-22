use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::bots;
use crate::state::AppState;

/// Revokes a bot: token invalid, account deactivated.
#[utoipa::path(delete, path = "/api/v1/admin/bots/{bot_id}", tag = "admin", summary = "Revokes a bot: token invalid, account deactivated.", params(("bot_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_revoke_bot_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(bot_id): ApiPath<UserId>,
) -> Result<StatusCode, ApiError> {
  bots::revoke_bot(&state, &caller, bot_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
