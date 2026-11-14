use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, PostMlsCommitRequest, PostMlsCommitResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::mls;
use crate::state::AppState;

/// Continues a strict channel's group from an earlier epoch with an admin's commit (e2ee.md §4.6.2).
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/mls/rewind", tag = "e2ee", summary = "Continues a strict channel's group from an earlier epoch with an admin's commit.", description = "Drops the queue's commits from that epoch on, every later entry and newer group information, sets the epoch back and appends the commit, in one transaction with an audit row. Needs `manage_channel_roles` and a device in the group. A group not in that epoch yet is an epoch conflict.", request_body = PostMlsCommitRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 201, body = PostMlsCommitResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn post_mls_rewind_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<PostMlsCommitRequest>,
) -> Result<(StatusCode, Json<PostMlsCommitResponse>), ApiError> {
  let response = mls::rewind(&state, &caller, conversation_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
