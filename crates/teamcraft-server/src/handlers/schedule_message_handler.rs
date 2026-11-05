use axum::Json;
use axum::extract::{Path, State};
use teamcraft_proto::{ConversationId, ErrorBody, ScheduleMessageRequest, ScheduledMessageResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::scheduled_messages;
use crate::state::AppState;

/// Holds a message for later; the server releases it at its time.
#[utoipa::path(post, path = "/api/v1/conversations/{id}/scheduled-messages", tag = "messages", summary = "Holds a message for later; the server releases it at its time.", params(("id" = ConversationId, Path)), request_body = ScheduleMessageRequest, responses((status = 200, body = ScheduledMessageResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn schedule_message_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  Path(conversation): Path<ConversationId>,
  Json(request): Json<ScheduleMessageRequest>,
) -> Result<Json<ScheduledMessageResponse>, ApiError> {
  let response = scheduled_messages::schedule_message(&state, &caller, conversation, request).await?;
  Ok(Json(response))
}
