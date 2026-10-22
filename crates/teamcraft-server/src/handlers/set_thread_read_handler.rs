use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, SetThreadReadRequest, ThreadId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::threads;
use crate::state::AppState;

/// Moves the read position of a followed thread forward.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/threads/{thread_id}/read", tag = "threads", summary = "Moves the read position of a followed thread forward.", request_body = SetThreadReadRequest, params(("conversation_id" = ConversationId, Path), ("thread_id" = ThreadId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn set_thread_read_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, thread_id)): ApiPath<(ConversationId, ThreadId)>,
  ApiJson(request): ApiJson<SetThreadReadRequest>,
) -> Result<StatusCode, ApiError> {
  threads::set_thread_read(&state, &caller, conversation_id, thread_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
