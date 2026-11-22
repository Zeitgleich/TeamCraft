use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, StickerPictureResponse, UploadStickerPictureRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::sticker_packs;
use crate::state::AppState;

/// Admins: uploads a picture for sticker packs, keyed by its SHA-256.
#[utoipa::path(post, path = "/api/v1/sticker-pictures", tag = "stickers", summary = "Admins: uploads a picture for sticker packs.", request_body = UploadStickerPictureRequest, responses((status = 200, body = StickerPictureResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 413, body = ErrorBody)), security(("bearer" = [])))]
pub async fn upload_sticker_picture_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<UploadStickerPictureRequest>,
) -> Result<Json<StickerPictureResponse>, ApiError> {
  Ok(Json(
    sticker_packs::upload_picture(&state, &caller, request).await?,
  ))
}
