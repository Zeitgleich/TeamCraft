use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ListMessagesQuery, ListMessagesResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiPath, ApiQuery};
use crate::services::messages;
use crate::state::AppState;

/// Returns a page of message history.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/messages", tag = "messages", summary = "Returns a page of message history.", params(("conversation_id" = ConversationId, Path), ListMessagesQuery), responses((status = 200, body = ListMessagesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_messages_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiQuery(query): ApiQuery<ListMessagesQuery>,
) -> Result<Json<ListMessagesResponse>, ApiError> {
  let response = messages::list_messages(&state, &caller, conversation_id, query).await?;
  Ok(Json(response))
}
