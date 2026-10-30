use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListRunningCallsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::calls;
use crate::state::AppState;

/// Lists the calls running in the caller's conversations.
#[utoipa::path(get, path = "/api/v1/calls", tag = "calls", summary = "Lists the calls running in the caller's conversations.", responses((status = 200, body = ListRunningCallsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_running_calls_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListRunningCallsResponse>, ApiError> {
  let response = calls::list_running_calls(&state, &caller).await?;
  Ok(Json(response))
}
