use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, ThreadId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::threads;
use crate::state::AppState;

/// Follows a thread.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/threads/{thread_id}/follow", tag = "threads", summary = "Follows a thread.", params(("conversation_id" = ConversationId, Path), ("thread_id" = ThreadId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn follow_thread_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, thread_id)): ApiPath<(ConversationId, ThreadId)>,
) -> Result<StatusCode, ApiError> {
  threads::follow_thread(&state, &caller, conversation_id, thread_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
