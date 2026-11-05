use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use teamcraft_proto::{ErrorBody, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::bots;
use crate::state::AppState;

/// Sets a bot's picture, the fallback of its sources. The body is a PNG, JPEG or WebP file whose
/// `Content-Type` matches it; the server re-encodes it as PNG.
#[utoipa::path(put, path = "/api/v1/admin/bots/{bot_id}/avatar", tag = "admin", summary = "Sets a bot's picture.", params(("bot_id" = UserId, Path)), request_body(content = Vec<u8>, content_type = "image/*"), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 413, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_put_bot_avatar_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(bot_id): ApiPath<UserId>,
  headers: HeaderMap,
  body: Bytes,
) -> Result<StatusCode, ApiError> {
  let declared_type = headers
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .unwrap_or_default();
  bots::put_bot_avatar(&state, &caller, bot_id, declared_type, body).await?;
  Ok(StatusCode::NO_CONTENT)
}
