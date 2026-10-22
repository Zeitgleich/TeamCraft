use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ConversationResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::conversations;
use crate::state::AppState;

/// Returns one conversation.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}", tag = "conversations", summary = "Returns one conversation.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ConversationResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_conversation_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<ConversationResponse>, ApiError> {
  let response = conversations::get_conversation(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
