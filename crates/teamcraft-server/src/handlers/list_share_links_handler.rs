use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListShareLinksResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::shares;
use crate::state::AppState;

/// Lists the caller's share links.
#[utoipa::path(get, path = "/api/v1/shares", tag = "shares", summary = "Lists the caller's share links.", responses((status = 200, body = ListShareLinksResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_share_links_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListShareLinksResponse>, ApiError> {
  let response = shares::list_shares(&state, &caller).await?;
  Ok(Json(response))
}
