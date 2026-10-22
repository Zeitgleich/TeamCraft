use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ConversationResponse, ConvertChannelRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::conversations;
use crate::state::AppState;

/// Makes a channel public or private.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/visibility", tag = "conversations", summary = "Makes a channel public or private.", request_body = ConvertChannelRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ConversationResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn convert_channel_privacy_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<ConvertChannelRequest>,
) -> Result<Json<ConversationResponse>, ApiError> {
  let response = conversations::convert_channel_privacy(&state, &caller, conversation_id, request).await?;
  Ok(Json(response))
}
