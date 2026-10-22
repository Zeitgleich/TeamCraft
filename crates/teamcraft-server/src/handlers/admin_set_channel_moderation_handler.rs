use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ChannelModeration, ConversationId, ErrorBody, SetChannelModerationRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::permission_scheme;
use crate::state::AppState;

/// Sets the moderation toggles of a channel for members and guests.
#[utoipa::path(put, path = "/api/v1/admin/conversations/{conversation_id}/moderation", tag = "admin", summary = "Sets the moderation toggles of a channel for members and guests.", request_body = SetChannelModerationRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ChannelModeration), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_set_channel_moderation_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<SetChannelModerationRequest>,
) -> Result<Json<ChannelModeration>, ApiError> {
  let response = permission_scheme::set_channel_moderation(&state, &caller, conversation_id, request).await?;
  Ok(Json(response))
}
