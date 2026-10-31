use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, ImportMemberStateRequest, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::import;
use crate::state::AppState;

/// Sets a member's read position, favourite and mute from the old system (import account only).
#[utoipa::path(put, path = "/api/v1/admin/import/conversations/{conversation_id}/members/{user_id}", tag = "admin", summary = "Sets a member's read position, favourite and mute from the old system (import account only).", request_body = ImportMemberStateRequest, params(("conversation_id" = ConversationId, Path), ("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_import_member_state_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath((conversation_id, user_id)): ApiPath<(ConversationId, UserId)>,
  ApiJson(request): ApiJson<ImportMemberStateRequest>,
) -> Result<StatusCode, ApiError> {
  import::set_member_state(&state, &caller, conversation_id, user_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
