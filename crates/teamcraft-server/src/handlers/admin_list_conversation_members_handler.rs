use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ListConversationMembersResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin;
use crate::state::AppState;

/// Lists the members of any channel.
#[utoipa::path(get, path = "/api/v1/admin/conversations/{conversation_id}/members", tag = "admin", summary = "Lists the members of any channel.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ListConversationMembersResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_conversation_members_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<ListConversationMembersResponse>, ApiError> {
  let response = admin::list_conversation_members(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
