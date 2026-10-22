use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, MessageResponse, SendMessageRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::messages;
use crate::state::AppState;

/// Stores an encrypted message and delivers it to the members.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/messages", tag = "messages", summary = "Stores an encrypted message and delivers it to the members.", request_body = SendMessageRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 201, body = MessageResponse), (status = 200, body = MessageResponse, description = "Already existed"), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn send_message_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<SendMessageRequest>,
) -> Result<(StatusCode, Json<MessageResponse>), ApiError> {
  let sent = messages::send_message(&state, &caller, conversation_id, request).await?;
  let status = if sent.is_new {
    StatusCode::CREATED
  } else {
    StatusCode::OK
  };
  Ok((status, Json(sent.message)))
}
