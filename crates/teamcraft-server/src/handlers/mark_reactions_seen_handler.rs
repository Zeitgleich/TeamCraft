use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, MarkReactionsSeenRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::members;
use crate::state::AppState;

/// Adds reactions to the caller's own messages to those they saw.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/reactions-seen", tag = "conversations", summary = "Adds reactions to the caller's own messages to those they saw.", request_body = MarkReactionsSeenRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn mark_reactions_seen_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<MarkReactionsSeenRequest>,
) -> Result<StatusCode, ApiError> {
  members::mark_reactions_seen(&state, &caller, conversation_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
