use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, RetentionStatusResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::retention;
use crate::state::AppState;

/// Returns a channel's retention and the workspace limit.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/retention", tag = "retention", summary = "Returns a channel's retention and the workspace limit.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = RetentionStatusResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_conversation_retention_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<RetentionStatusResponse>, ApiError> {
  let response = retention::get_conversation_retention(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
