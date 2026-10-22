use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, MeetingReplyResponse};

use crate::auth::InboxCaller;
use crate::error::ApiError;
use crate::services::meetings;
use crate::state::AppState;

/// Internal: an iMIP REPLY (`text/calendar`, method REPLY) received by e-mail. The caller (the
/// inbox handler that receives the mail) vouches for the sender.
#[utoipa::path(post, path = "/api/internal/v1/meetings/replies", tag = "internal", summary = "Internal: applies an iMIP REPLY received by e-mail.", request_body(content = String, content_type = "text/calendar"), responses((status = 200, body = MeetingReplyResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn receive_meeting_reply_handler(
  State(state): State<AppState>,
  _caller: InboxCaller,
  body: String,
) -> Result<Json<MeetingReplyResponse>, ApiError> {
  let response = meetings::receive_reply(&state, &body).await?;
  Ok(Json(response))
}
