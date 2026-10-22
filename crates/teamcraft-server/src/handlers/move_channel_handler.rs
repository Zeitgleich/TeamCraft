use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ConversationResponse, ErrorBody, MoveChannelRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::conversations;
use crate::state::AppState;

/// Moves a channel to another space.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/space", tag = "conversations", summary = "Moves a channel to another space.", request_body = MoveChannelRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ConversationResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn move_channel_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<MoveChannelRequest>,
) -> Result<Json<ConversationResponse>, ApiError> {
  let response = conversations::move_channel(&state, &caller, conversation_id, request).await?;
  Ok(Json(response))
}
