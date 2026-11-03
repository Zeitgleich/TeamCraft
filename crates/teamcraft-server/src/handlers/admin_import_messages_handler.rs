use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ImportMessagesRequest, ImportMessagesResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::import;
use crate::state::AppState;

/// Stores imported messages before the conversation's live ones, without notifying anybody (import account only).
#[utoipa::path(post, path = "/api/v1/admin/import/conversations/{conversation_id}/messages", tag = "admin", summary = "Stores imported messages before the conversation's live ones, without notifying anybody (import account only).", request_body = ImportMessagesRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ImportMessagesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_import_messages_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<ImportMessagesRequest>,
) -> Result<Json<ImportMessagesResponse>, ApiError> {
  Ok(Json(
    import::import_messages(&state, &caller, conversation_id, request).await?,
  ))
}
