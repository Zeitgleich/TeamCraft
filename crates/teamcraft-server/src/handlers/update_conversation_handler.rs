use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ConversationResponse, ErrorBody, UpdateConversationRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::conversations;
use crate::state::AppState;

/// Changes a channel's settings, moderation or archive state.
#[utoipa::path(patch, path = "/api/v1/conversations/{conversation_id}", tag = "conversations", summary = "Changes a channel's settings, moderation or archive state.", request_body = UpdateConversationRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ConversationResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_conversation_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<UpdateConversationRequest>,
) -> Result<Json<ConversationResponse>, ApiError> {
  let response = conversations::update_conversation(&state, &caller, conversation_id, request).await?;
  Ok(Json(response))
}
