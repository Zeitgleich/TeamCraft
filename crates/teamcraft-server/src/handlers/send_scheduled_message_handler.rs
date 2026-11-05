use axum::Json;
use axum::extract::{Path, State};
use teamcraft_proto::{ErrorBody, ScheduledMessageId, SendScheduledMessageResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::scheduled_messages;
use crate::state::AppState;

/// Releases one of the caller's held messages now.
#[utoipa::path(post, path = "/api/v1/me/scheduled-messages/{id}/send", tag = "messages", summary = "Releases one of the caller's held messages now.", params(("id" = ScheduledMessageId, Path)), responses((status = 200, body = SendScheduledMessageResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn send_scheduled_message_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  Path(id): Path<ScheduledMessageId>,
) -> Result<Json<SendScheduledMessageResponse>, ApiError> {
  let response = scheduled_messages::send_scheduled_now(&state, &caller, id).await?;
  Ok(Json(response))
}
