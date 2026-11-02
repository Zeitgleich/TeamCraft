use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::conversations;
use crate::state::AppState;

/// Deletes a channel for everybody.
#[utoipa::path(delete, path = "/api/v1/conversations/{conversation_id}", tag = "conversations", summary = "Deletes a channel for everybody.", description = "Archived or not; needs `delete_channel`. Its messages, MLS state and files go, and the members' devices drop it with their local history (e2ee.md §4.6). `404` once it is gone.", params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_conversation_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<StatusCode, ApiError> {
  conversations::delete_channel(&state, &caller, conversation_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
