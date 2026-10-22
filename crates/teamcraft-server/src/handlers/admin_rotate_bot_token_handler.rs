use axum::Json;
use axum::extract::State;
use teamcraft_proto::{BotTokenResponse, ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::bots;
use crate::state::AppState;

/// Issues a new token for a bot; the old one stops working.
#[utoipa::path(post, path = "/api/v1/admin/bots/{bot_id}/token", tag = "admin", summary = "Issues a new token for a bot; the old one stops working.", params(("bot_id" = UserId, Path)), responses((status = 200, body = BotTokenResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_rotate_bot_token_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(bot_id): ApiPath<UserId>,
) -> Result<Json<BotTokenResponse>, ApiError> {
  let response = bots::rotate_token(&state, &caller, bot_id).await?;
  Ok(Json(response))
}
