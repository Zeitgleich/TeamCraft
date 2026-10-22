use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, PostMlsCommitRequest, PostMlsCommitResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::mls;
use crate::state::AppState;

/// Appends an MLS commit (and welcomes); the first commit per epoch wins.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/mls/commits", tag = "e2ee", summary = "Appends an MLS commit (and welcomes); the first commit per epoch wins.", request_body = PostMlsCommitRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 201, body = PostMlsCommitResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn post_mls_commit_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<PostMlsCommitRequest>,
) -> Result<(StatusCode, Json<PostMlsCommitResponse>), ApiError> {
  let response = mls::post_commit(&state, &caller, conversation_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
