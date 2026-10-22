use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AssignChannelOwnerRequest, ConversationId, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::admin;
use crate::state::AppState;

/// Makes a member the admin of a channel without one.
#[utoipa::path(post, path = "/api/v1/admin/conversations/{conversation_id}/owner", tag = "admin", summary = "Makes a member the admin of a channel without one.", request_body = AssignChannelOwnerRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_assign_channel_owner_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<AssignChannelOwnerRequest>,
) -> Result<StatusCode, ApiError> {
  admin::assign_channel_owner(&state, &caller, conversation_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
