use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListBotsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::bots;
use crate::state::AppState;

/// Lists the bot accounts.
#[utoipa::path(get, path = "/api/v1/admin/bots", tag = "admin", summary = "Lists the bot accounts.", responses((status = 200, body = ListBotsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_bots_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListBotsResponse>, ApiError> {
  let response = bots::list_bots(&state, &caller).await?;
  Ok(Json(response))
}
