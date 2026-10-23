use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, PutMlsGroupInfoRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::mls;
use crate::state::AppState;

/// Publishes the MLS group information for the current epoch.
#[utoipa::path(put, path = "/api/v1/conversations/{conversation_id}/mls/group-info", tag = "e2ee", summary = "Publishes the MLS group information for the current epoch.", request_body = PutMlsGroupInfoRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_mls_group_info_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<PutMlsGroupInfoRequest>,
) -> Result<StatusCode, ApiError> {
  mls::put_group_info(&state, &caller, conversation_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
