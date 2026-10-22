use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ConversationId, ErrorBody, MlsMessageResponse, PostMlsMessageRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::mls;
use crate::state::AppState;

/// Appends an MLS proposal or application message for the current epoch.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/mls/messages", tag = "e2ee", summary = "Appends an MLS proposal or application message for the current epoch.", request_body = PostMlsMessageRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 201, body = MlsMessageResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn post_mls_message_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<PostMlsMessageRequest>,
) -> Result<(StatusCode, Json<MlsMessageResponse>), ApiError> {
  let response = mls::post_message(&state, &caller, conversation_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
