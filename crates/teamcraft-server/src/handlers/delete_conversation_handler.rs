use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, DeleteConversationRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiOptionalJson, ApiPath};
use crate::services::conversations;
use crate::state::AppState;

/// Deletes a channel for everybody.
#[utoipa::path(delete, path = "/api/v1/conversations/{conversation_id}", tag = "conversations", summary = "Deletes a channel for everybody.", description = "Archived or not; needs `delete_channel`. Its messages, MLS state and files go, and the members' devices drop it with their local history (e2ee.md §4.6). `404` once it is gone. The body is optional: a signed deletion, for that group's current epoch (`409 epoch_conflict` otherwise), lets the devices drop it at once; without one they keep the history for a while.", params(("conversation_id" = ConversationId, Path)), request_body(content = Option<DeleteConversationRequest>), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_conversation_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiOptionalJson(request): ApiOptionalJson<DeleteConversationRequest>,
) -> Result<StatusCode, ApiError> {
  conversations::delete_channel(&state, &caller, conversation_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
