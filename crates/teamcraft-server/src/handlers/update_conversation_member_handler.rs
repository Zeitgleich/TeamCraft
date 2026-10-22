use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, UpdateConversationMemberRequest, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::members;
use crate::state::AppState;

/// Changes a member's role or the caller's own preferences.
#[utoipa::path(patch, path = "/api/v1/conversations/{conversation_id}/members/{user_id}", tag = "conversations", summary = "Changes a member's role or the caller's own preferences.", request_body = UpdateConversationMemberRequest, params(("conversation_id" = ConversationId, Path), ("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_conversation_member_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, user_id)): ApiPath<(ConversationId, UserId)>,
  ApiJson(request): ApiJson<UpdateConversationMemberRequest>,
) -> Result<StatusCode, ApiError> {
  members::update_member(&state, &caller, conversation_id, user_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
