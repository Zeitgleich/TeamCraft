use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SaveStickerPackRequest, StickerPackKey, StickerPackResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::sticker_packs;
use crate::state::AppState;

/// Admins: creates a sticker pack or replaces it with a newer version their device signed.
#[utoipa::path(put, path = "/api/v1/sticker-packs/{pack}", tag = "stickers", summary = "Admins: creates a sticker pack or saves a newer version of it.", params(("pack" = String, Path, description = "The pack's key, 32 hex characters")), request_body = SaveStickerPackRequest, responses((status = 200, body = StickerPackResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn save_sticker_pack_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(pack): ApiPath<StickerPackKey>,
  ApiJson(request): ApiJson<SaveStickerPackRequest>,
) -> Result<Json<StickerPackResponse>, ApiError> {
  Ok(Json(sticker_packs::save(&state, &caller, pack, request).await?))
}
