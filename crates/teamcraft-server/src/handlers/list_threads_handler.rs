use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListThreadsQuery, ListThreadsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::threads;
use crate::state::AppState;

/// Lists the followed threads with unread counts (threads inbox).
#[utoipa::path(get, path = "/api/v1/threads", tag = "threads", summary = "Lists the followed threads with unread counts (threads inbox).", params(ListThreadsQuery), responses((status = 200, body = ListThreadsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_threads_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<ListThreadsQuery>,
) -> Result<Json<ListThreadsResponse>, ApiError> {
  let response = threads::list_threads(&state, &caller, query).await?;
  Ok(Json(response))
}
