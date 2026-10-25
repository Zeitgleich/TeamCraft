use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, MoveCallRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::calls;
use crate::state::AppState;

/// Moves the conversation's DM call into a meeting the caller started for it.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/call/move", tag = "calls", summary = "Moves the conversation's DM call into a meeting the caller started for it.", params(("conversation_id" = ConversationId, Path)), request_body = MoveCallRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn move_call_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<MoveCallRequest>,
) -> Result<StatusCode, ApiError> {
  calls::move_call(&state, &caller, conversation_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
