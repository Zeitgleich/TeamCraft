use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AddConversationMemberRequest, ConversationId, ConversationMemberResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::members;
use crate::state::AppState;

/// Adds a space member to a channel.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/members", tag = "conversations", summary = "Adds a space member to a channel.", request_body = AddConversationMemberRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 201, body = ConversationMemberResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn add_conversation_member_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<AddConversationMemberRequest>,
) -> Result<(StatusCode, Json<ConversationMemberResponse>), ApiError> {
  let response = members::add_member(&state, &caller, conversation_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
