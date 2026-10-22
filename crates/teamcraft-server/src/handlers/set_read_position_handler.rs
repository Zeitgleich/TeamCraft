use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, SetReadPositionRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::members;
use crate::state::AppState;

/// Moves the caller's read position forward.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/read-position", tag = "conversations", summary = "Moves the caller's read position forward.", request_body = SetReadPositionRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn set_read_position_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<SetReadPositionRequest>,
) -> Result<StatusCode, ApiError> {
  members::set_read_position(&state, &caller, conversation_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
