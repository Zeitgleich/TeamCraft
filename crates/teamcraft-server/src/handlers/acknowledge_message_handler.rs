use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, MessageId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::urgent;
use crate::state::AppState;

/// Acknowledges an urgent message: ends its repeated pushes and tells the sender.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/messages/{message_id}/acknowledgement", tag = "notifications", summary = "Acknowledges an urgent message: ends its repeated pushes and tells the sender.", params(("conversation_id" = ConversationId, Path), ("message_id" = MessageId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn acknowledge_message_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, message_id)): ApiPath<(ConversationId, MessageId)>,
) -> Result<StatusCode, ApiError> {
  urgent::acknowledge(&state, &caller, conversation_id, message_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
