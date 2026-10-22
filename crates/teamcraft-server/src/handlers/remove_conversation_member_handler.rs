use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::members;
use crate::state::AppState;

/// Removes a member from a channel (or leaves).
#[utoipa::path(delete, path = "/api/v1/conversations/{conversation_id}/members/{user_id}", tag = "conversations", summary = "Removes a member from a channel (or leaves).", params(("conversation_id" = ConversationId, Path), ("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn remove_conversation_member_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, user_id)): ApiPath<(ConversationId, UserId)>,
) -> Result<StatusCode, ApiError> {
  members::remove_member(&state, &caller, conversation_id, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
