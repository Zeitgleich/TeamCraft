use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ConversationId, ErrorBody, ListAcknowledgementsResponse, MessageId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::urgent;
use crate::state::AppState;

/// Lists who acknowledged an urgent message.
#[utoipa::path(get, path = "/api/v1/conversations/{conversation_id}/messages/{message_id}/acknowledgements", tag = "notifications", summary = "Lists who acknowledged an urgent message.", params(("conversation_id" = ConversationId, Path), ("message_id" = MessageId, Path)), responses((status = 200, body = ListAcknowledgementsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_acknowledgements_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((conversation_id, message_id)): ApiPath<(ConversationId, MessageId)>,
) -> Result<Json<ListAcknowledgementsResponse>, ApiError> {
  let response = urgent::list_acknowledgements(&state, &caller, conversation_id, message_id).await?;
  Ok(Json(response))
}
