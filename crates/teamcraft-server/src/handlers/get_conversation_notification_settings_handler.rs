use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ConversationNotificationSettings, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::notifications;
use crate::state::AppState;

/// Returns the caller's notification level and mute time for a conversation.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/notification-settings", tag = "notifications", summary = "Returns the caller's notification level and mute time for a conversation.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ConversationNotificationSettings), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_conversation_notification_settings_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<ConversationNotificationSettings>, ApiError> {
  let response = notifications::get_conversation_settings(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
