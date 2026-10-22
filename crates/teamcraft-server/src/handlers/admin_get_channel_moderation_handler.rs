use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ChannelModeration, ConversationId, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::permission_scheme;
use crate::state::AppState;

/// Returns what members and guests may not do in a channel.
#[utoipa::path(get, path = "/api/v1/admin/conversations/{conversation_id}/moderation", tag = "admin", summary = "Returns what members and guests may not do in a channel.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ChannelModeration), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_channel_moderation_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<ChannelModeration>, ApiError> {
  let response = permission_scheme::get_channel_moderation(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
