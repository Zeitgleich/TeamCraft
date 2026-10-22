use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ConversationNotificationSettings, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::notifications;
use crate::state::AppState;

/// Sets the caller's notification level and mute time for a conversation.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/notification-settings", tag = "notifications", summary = "Sets the caller's notification level and mute time for a conversation.", request_body = ConversationNotificationSettings, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ConversationNotificationSettings), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_conversation_notification_settings_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<ConversationNotificationSettings>,
) -> Result<Json<ConversationNotificationSettings>, ApiError> {
  let response = notifications::put_conversation_settings(&state, &caller, conversation_id, request).await?;
  Ok(Json(response))
}
