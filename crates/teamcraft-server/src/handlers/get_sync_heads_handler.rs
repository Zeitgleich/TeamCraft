use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SyncHeadsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::sync;
use crate::state::AppState;

/// Returns where each of the caller's conversations and spaces stands, so a client coming back reads only what changed.
#[utoipa::path(get, path = "/api/v1/sync/heads", tag = "sync", summary = "Returns where each of the caller's conversations and spaces stands, so a client coming back reads only what changed.", responses((status = 200, body = SyncHeadsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_sync_heads_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<SyncHeadsResponse>, ApiError> {
  let response = sync::sync_heads(&state, &caller).await?;
  Ok(Json(response))
}
