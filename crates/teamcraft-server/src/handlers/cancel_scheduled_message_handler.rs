use axum::extract::{Path, State};
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, ScheduledMessageId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::scheduled_messages;
use crate::state::AppState;

/// Cancels one of the caller's held messages.
#[utoipa::path(delete, path = "/api/v1/me/scheduled-messages/{id}", tag = "messages", summary = "Cancels one of the caller's held messages.", params(("id" = ScheduledMessageId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn cancel_scheduled_message_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  Path(id): Path<ScheduledMessageId>,
) -> Result<StatusCode, ApiError> {
  scheduled_messages::cancel_scheduled_message(&state, &caller, id).await?;
  Ok(StatusCode::NO_CONTENT)
}
