use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CallStateResponse, ConversationId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::calls;
use crate::state::AppState;

/// Returns the call that is running in the conversation.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/call", tag = "calls", summary = "Returns the call that is running in the conversation.", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = CallStateResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_call_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<CallStateResponse>, ApiError> {
  let response = calls::get_call(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
