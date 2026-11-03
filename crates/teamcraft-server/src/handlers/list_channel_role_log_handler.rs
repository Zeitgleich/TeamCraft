use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ListChannelRoleLogQuery, ListChannelRoleLogResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiPath, ApiQuery};
use crate::services::channel_roles;
use crate::state::AppState;

/// Returns a channel's role log entries after a version.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/role-log", tag = "conversations", summary = "Returns a channel's role log entries after a version.", params(("conversation_id" = ConversationId, Path), ListChannelRoleLogQuery), responses((status = 200, body = ListChannelRoleLogResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_channel_role_log_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiQuery(query): ApiQuery<ListChannelRoleLogQuery>,
) -> Result<Json<ListChannelRoleLogResponse>, ApiError> {
  let response = channel_roles::list(&state, &caller, conversation_id, query).await?;
  Ok(Json(response))
}
