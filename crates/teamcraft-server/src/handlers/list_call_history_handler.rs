use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListCallHistoryQuery, ListCallHistoryResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::call_history;
use crate::state::AppState;

/// Lists the caller's calls, newest first; missed ones are marked.
#[utoipa::path(get, path = "/api/v1/calls/history", tag = "calls", summary = "Lists the caller's calls, newest first; missed ones are marked.", params(ListCallHistoryQuery), responses((status = 200, body = ListCallHistoryResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_call_history_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<ListCallHistoryQuery>,
) -> Result<Json<ListCallHistoryResponse>, ApiError> {
  let response = call_history::list_call_history(&state, &caller, query).await?;
  Ok(Json(response))
}
