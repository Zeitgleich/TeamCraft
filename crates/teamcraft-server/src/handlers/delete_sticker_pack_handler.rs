use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, StickerPackKey};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::sticker_packs;
use crate::state::AppState;

/// Admins: deletes a sticker pack. Messages that sent its stickers stay unchanged.
#[utoipa::path(delete, path = "/api/v1/sticker-packs/{pack}", tag = "stickers", summary = "Admins: deletes a sticker pack.", params(("pack" = String, Path, description = "The pack's key, 32 hex characters")), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_sticker_pack_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(pack): ApiPath<StickerPackKey>,
) -> Result<StatusCode, ApiError> {
  sticker_packs::delete(&state, &caller, pack).await?;
  Ok(StatusCode::NO_CONTENT)
}
