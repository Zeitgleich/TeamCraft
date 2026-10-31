use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::import;
use crate::state::AppState;

/// Drops the import account's membership of a DM or notes once it left the group (import account only).
#[utoipa::path(delete, path = "/api/v1/admin/import/conversations/{conversation_id}/importer", tag = "admin", summary = "Drops the import account's membership of a DM or notes once it left the group (import account only).", params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_remove_importer_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<StatusCode, ApiError> {
  import::remove_importer(&state, &caller, conversation_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
