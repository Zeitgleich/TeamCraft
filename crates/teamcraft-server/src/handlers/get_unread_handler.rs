use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, UnreadResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::threads;
use crate::state::AppState;

/// Returns unread and mention counts of all conversations and followed threads.
#[utoipa::path(get, path = "/api/v1/unread", tag = "threads", summary = "Returns unread and mention counts of all conversations and followed threads.", responses((status = 200, body = UnreadResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_unread_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<UnreadResponse>, ApiError> {
  let response = threads::get_unread(&state, &caller).await?;
  Ok(Json(response))
}
