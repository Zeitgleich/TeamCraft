use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::calls;
use crate::state::AppState;

/// Takes all of the caller's devices out of the conversation's call ("Leave on all devices").
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/call/leave", tag = "calls", summary = "Takes all of the caller's devices out of the conversation's call.", params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn leave_call_everywhere_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<StatusCode, ApiError> {
  calls::leave_call_everywhere(&state, &caller, conversation_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
