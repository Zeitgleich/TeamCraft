use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CallJoinTokenResponse, ConversationId, CreateCallJoinTokenRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::calls;
use crate::state::AppState;

/// Issues a short-lived token for joining the conversation's call on the SFU.
#[utoipa::path(post, path = "/api/v1/conversations/{conversation_id}/call/join-token", tag = "calls", summary = "Issues a short-lived token for joining the conversation's call on the SFU.", request_body = CreateCallJoinTokenRequest, params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = CallJoinTokenResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_call_join_token_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
  ApiJson(request): ApiJson<CreateCallJoinTokenRequest>,
) -> Result<Json<CallJoinTokenResponse>, ApiError> {
  let response = calls::create_join_token(&state, &caller, conversation_id, request).await?;
  Ok(Json(response))
}
