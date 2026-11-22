use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CustomEmojiHash, ErrorBody, StickerPictureDownloadResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::sticker_packs;
use crate::state::AppState;

/// A download URL of a sticker picture (everybody signed in).
#[utoipa::path(get, path = "/api/v1/sticker-pictures/{sha256}/download", tag = "stickers", summary = "A download URL of a sticker picture.", params(("sha256" = String, Path, description = "The picture's SHA-256, 64 hex characters")), responses((status = 200, body = StickerPictureDownloadResponse), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn download_sticker_picture_handler(
  State(state): State<AppState>,
  _caller: AuthenticatedUser,
  ApiPath(sha256): ApiPath<CustomEmojiHash>,
) -> Result<Json<StickerPictureDownloadResponse>, ApiError> {
  Ok(Json(sticker_packs::download_picture(&state, sha256).await?))
}
