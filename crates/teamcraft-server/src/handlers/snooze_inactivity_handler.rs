use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ChannelInactivityStatus, ConversationId, ErrorBody, SnoozeInactivityRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::inactivity;
use crate::state::AppState;

/// Keeps a channel open for a while: the inactivity policy leaves it alone.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/inactivity/snooze", tag = "conversations", summary = "Keeps a channel open for a while: the inactivity policy leaves it alone.", description = "Needs `manage_channel_properties`. Nothing is noticed, archived or deleted until the snooze ends (the policy's `snooze_days` unless `days` says otherwise, at most 365), and a notice already sent is forgotten, so a channel still quiet afterwards is noticed afresh. Also defers the deletion of a channel the policy archived. Audit `channel_inactivity_snoozed`.", request_body = SnoozeInactivityRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ChannelInactivityStatus), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn snooze_inactivity_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<SnoozeInactivityRequest>,
) -> Result<Json<ChannelInactivityStatus>, ApiError> {
  let status = inactivity::snooze(&state, &caller, conversation_id, request).await?;
  Ok(Json(status))
}
