use axum::Json;
use axum::extract::{Path, State};
use teamcraft_proto::{
  ErrorBody, ScheduledMessageId, ScheduledMessageResponse, UpdateScheduledMessageRequest,
};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::scheduled_messages;
use crate::state::AppState;

/// Changes the time and the sealed text of one of the caller's held messages.
#[utoipa::path(put, path = "/api/v1/me/scheduled-messages/{id}", tag = "messages", summary = "Changes the time and the sealed text of one of the caller's held messages.", params(("id" = ScheduledMessageId, Path)), request_body = UpdateScheduledMessageRequest, responses((status = 200, body = ScheduledMessageResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_scheduled_message_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  Path(id): Path<ScheduledMessageId>,
  Json(request): Json<UpdateScheduledMessageRequest>,
) -> Result<Json<ScheduledMessageResponse>, ApiError> {
  let response = scheduled_messages::update_scheduled_message(&state, &caller, id, request).await?;
  Ok(Json(response))
}
