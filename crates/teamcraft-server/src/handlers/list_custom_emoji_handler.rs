use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListCustomEmojiResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::custom_emoji;
use crate::state::AppState;

/// The workspace's custom emoji with their signed documents, and the limits for new ones.
#[utoipa::path(get, path = "/api/v1/custom-emoji", tag = "custom emoji", summary = "The workspace's custom emoji with their signed documents, and the limits for new ones.", responses((status = 200, body = ListCustomEmojiResponse), (status = 401, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_custom_emoji_handler(
  State(state): State<AppState>,
  _caller: AuthenticatedUser,
) -> Result<Json<ListCustomEmojiResponse>, ApiError> {
  Ok(Json(custom_emoji::list(&state).await?))
}
