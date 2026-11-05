use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListScheduledMessagesResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::scheduled_messages;
use crate::state::AppState;

/// The caller's held messages, soonest first.
#[utoipa::path(get, path = "/api/v1/me/scheduled-messages", tag = "messages", summary = "The caller's held messages, soonest first.", responses((status = 200, body = ListScheduledMessagesResponse), (status = 401, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_scheduled_messages_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListScheduledMessagesResponse>, ApiError> {
  let response = scheduled_messages::list_scheduled_messages(&state, &caller).await?;
  Ok(Json(response))
}
