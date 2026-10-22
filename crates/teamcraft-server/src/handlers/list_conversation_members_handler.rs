use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ListConversationMembersResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::members;
use crate::state::AppState;

/// Lists the members of a conversation.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/members", tag = "conversations", summary = "Lists the members of a conversation.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ListConversationMembersResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_conversation_members_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<ListConversationMembersResponse>, ApiError> {
  let response = members::list_members(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
