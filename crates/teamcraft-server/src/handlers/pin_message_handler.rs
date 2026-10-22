use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, MessageId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::retention;
use crate::state::AppState;

/// Pins a message: it survives retention.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/messages/{message_id}/pin", tag = "retention", summary = "Pins a message: it survives retention.", params(("conversation_id" = ConversationId, Path), ("message_id" = MessageId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn pin_message_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, message_id)): ApiPath<(ConversationId, MessageId)>,
) -> Result<StatusCode, ApiError> {
  retention::pin_message(&state, &caller, conversation_id, message_id, true).await?;
  Ok(StatusCode::NO_CONTENT)
}
