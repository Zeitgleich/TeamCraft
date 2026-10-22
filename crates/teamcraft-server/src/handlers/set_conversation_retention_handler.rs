use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, RetentionStatusResponse, SetRetentionRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::retention;
use crate::state::AppState;

/// Sets a channel's retention within the workspace limit.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/retention", tag = "retention", summary = "Sets a channel's retention within the workspace limit.", request_body = SetRetentionRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = RetentionStatusResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn set_conversation_retention_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<SetRetentionRequest>,
) -> Result<Json<RetentionStatusResponse>, ApiError> {
  let response = retention::set_conversation_retention(&state, &caller, conversation_id, request).await?;
  Ok(Json(response))
}
