use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, MeetingReplyResponse};

use crate::error::ApiError;
use crate::extract::BearerToken;
use crate::services::meetings;
use crate::state::AppState;

/// Internal: an iMIP REPLY (`text/calendar`, method REPLY) received by e-mail. The caller (the
/// inbox handler that receives the mail) vouches for the sender.
#[utoipa::path(post, path = "/api/internal/v1/meetings/replies", tag = "internal", summary = "Internal: applies an iMIP REPLY received by e-mail.", request_body(content = String, content_type = "text/calendar"), responses((status = 200, body = MeetingReplyResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn receive_meeting_reply_handler(
  State(state): State<AppState>,
  BearerToken(maybe_token): BearerToken,
  body: String,
) -> Result<Json<MeetingReplyResponse>, ApiError> {
  meetings::authenticate_inbox(&state, maybe_token.as_deref())?;
  let response = meetings::receive_reply(&state, &body).await?;
  Ok(Json(response))
}
