use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::import;
use crate::state::AppState;

/// Marks a conversation's import as finished and tells its members (import account only).
#[utoipa::path(post, path = "/api/v1/admin/import/conversations/{conversation_id}/finish", tag = "admin", summary = "Marks a conversation's import as finished and tells its members (import account only).", params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_finish_import_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<StatusCode, ApiError> {
  import::finish(&state, &caller, conversation_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
