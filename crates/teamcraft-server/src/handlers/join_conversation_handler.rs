use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::members;
use crate::state::AppState;

/// Joins a public channel.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/join", tag = "conversations", summary = "Joins a public channel.", params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn join_conversation_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<StatusCode, ApiError> {
  members::join_conversation(&state, &caller, conversation_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
