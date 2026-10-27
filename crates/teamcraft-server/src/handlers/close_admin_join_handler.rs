use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin_join_requests;
use crate::state::AppState;

/// Declines an admin's request to join (an owner of the channel) or withdraws it (the admin).
#[utoipa::path(delete, path = "/api/v1/conversations/{conversation_id}/admin-join-requests/{user_id}", tag = "conversations", summary = "Declines an admin's request to join, or withdraws one's own.", description = "Idempotent: `204` also when the request is gone already (decided on another device, expired).", params(("conversation_id" = ConversationId, Path), ("user_id" = UserId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn close_admin_join_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, user_id)): ApiPath<(ConversationId, UserId)>,
) -> Result<StatusCode, ApiError> {
  admin_join_requests::close(&state, &caller, conversation_id, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
