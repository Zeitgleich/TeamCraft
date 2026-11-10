use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CustomEmojiDownloadResponse, CustomEmojiId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::custom_emoji;
use crate::state::AppState;

/// A download URL of a custom emoji's picture (everybody signed in).
#[utoipa::path(get, path = "/api/v1/custom-emoji/{emoji_id}/download", tag = "custom emoji", summary = "A download URL of a custom emoji's picture.", params(("emoji_id" = CustomEmojiId, Path)), responses((status = 200, body = CustomEmojiDownloadResponse), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn download_custom_emoji_handler(
  State(state): State<AppState>,
  _caller: AuthenticatedUser,
  ApiPath(emoji_id): ApiPath<CustomEmojiId>,
) -> Result<Json<CustomEmojiDownloadResponse>, ApiError> {
  Ok(Json(custom_emoji::download(&state, emoji_id).await?))
}
