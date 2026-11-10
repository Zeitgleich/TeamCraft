use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CustomEmojiId, CustomEmojiResponse, ErrorBody, RenameCustomEmojiRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::custom_emoji;
use crate::state::AppState;

/// Admins: renames a custom emoji with a document signed over the same picture.
#[utoipa::path(put, path = "/api/v1/custom-emoji/{emoji_id}", tag = "custom emoji", summary = "Admins: renames a custom emoji with a document signed over the same picture.", params(("emoji_id" = CustomEmojiId, Path)), request_body = RenameCustomEmojiRequest, responses((status = 200, body = CustomEmojiResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn rename_custom_emoji_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(emoji_id): ApiPath<CustomEmojiId>,
  ApiJson(request): ApiJson<RenameCustomEmojiRequest>,
) -> Result<Json<CustomEmojiResponse>, ApiError> {
  Ok(Json(
    custom_emoji::rename(&state, &caller, emoji_id, request).await?,
  ))
}
