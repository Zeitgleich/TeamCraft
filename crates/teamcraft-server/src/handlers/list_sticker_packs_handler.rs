use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListStickerPacksResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::sticker_packs;
use crate::state::AppState;

/// The workspace's sticker packs with their signed documents, and the limits for new ones.
#[utoipa::path(get, path = "/api/v1/sticker-packs", tag = "stickers", summary = "The workspace's sticker packs with their signed documents, and the limits for new ones.", responses((status = 200, body = ListStickerPacksResponse), (status = 401, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_sticker_packs_handler(
  State(state): State<AppState>,
  _caller: AuthenticatedUser,
) -> Result<Json<ListStickerPacksResponse>, ApiError> {
  Ok(Json(sticker_packs::list(&state).await?))
}
