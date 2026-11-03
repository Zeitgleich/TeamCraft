use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AppendChannelRoleLogRequest, ChannelRoleLogEntryResponse, ConversationId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::channel_roles;
use crate::state::AppState;

/// Appends a signed entry to a channel's role log.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/role-log", tag = "conversations", summary = "Appends a signed entry to a channel's role log.", request_body = AppendChannelRoleLogRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 201, body = ChannelRoleLogEntryResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn append_channel_role_log_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<AppendChannelRoleLogRequest>,
) -> Result<(StatusCode, Json<ChannelRoleLogEntryResponse>), ApiError> {
  let response = channel_roles::append(&state, &caller, conversation_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
