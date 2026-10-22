use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ReadReceiptsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::notifications;
use crate::state::AppState;

/// Returns who read up to where in a DM or group DM (reciprocal).
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/read-receipts", tag = "notifications", summary = "Returns who read up to where in a DM or group DM (reciprocal).", params(("conversation_id" = ConversationId, Path)), responses((status = 200, body = ReadReceiptsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_read_receipts_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(conversation_id): ApiPath<ConversationId>,
) -> Result<Json<ReadReceiptsResponse>, ApiError> {
  let response = notifications::get_read_receipts(&state, &caller, conversation_id).await?;
  Ok(Json(response))
}
