use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CustomEmojiId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::custom_emoji;
use crate::state::AppState;

/// Admins: deletes a custom emoji and its picture. Messages that use it stay unchanged.
#[utoipa::path(delete, path = "/api/v1/custom-emoji/{emoji_id}", tag = "custom emoji", summary = "Admins: deletes a custom emoji and its picture.", params(("emoji_id" = CustomEmojiId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_custom_emoji_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(emoji_id): ApiPath<CustomEmojiId>,
) -> Result<StatusCode, ApiError> {
  custom_emoji::delete(&state, &caller, emoji_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
