use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListConversationsQuery, ListConversationsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::conversations;
use crate::state::AppState;

/// Lists the caller's conversations, or the joinable channels of a space.
#[utoipa::path(get, path = "/api/v1/conversations", tag = "conversations", summary = "Lists the caller's conversations, or the joinable channels of a space.", params(ListConversationsQuery), responses((status = 200, body = ListConversationsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_conversations_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<ListConversationsQuery>,
) -> Result<Json<ListConversationsResponse>, ApiError> {
  let response = conversations::list_conversations(&state, &caller, query).await?;
  Ok(Json(response))
}
