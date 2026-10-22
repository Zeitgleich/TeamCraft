use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use teamcraft_proto::{ErrorBody, MeetingReplyResponse};

use crate::auth::InboxCaller;
use crate::error::ApiError;
use crate::services::meetings;
use crate::state::AppState;

/// Internal: an iMIP REPLY (`text/calendar`, method REPLY) received by e-mail. The caller (the
/// inbox handler that receives the mail) vouches for the sender with `X-Envelope-Sender`; the reply
/// must answer for exactly that address.
#[utoipa::path(post, path = "/api/internal/v1/meetings/replies", tag = "internal", summary = "Internal: applies an iMIP REPLY received by e-mail.", request_body(content = String, content_type = "text/calendar"), responses((status = 200, body = MeetingReplyResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn receive_meeting_reply_handler(
  State(state): State<AppState>,
  _caller: InboxCaller,
  headers: HeaderMap,
  body: String,
) -> Result<Json<MeetingReplyResponse>, ApiError> {
  let sender = headers
    .get("x-envelope-sender")
    .and_then(|value| value.to_str().ok())
    .filter(|sender| !sender.trim().is_empty())
    .ok_or_else(|| ApiError::InvalidRequest("the X-Envelope-Sender header is required".into()))?;
  let response = meetings::receive_reply(&state, &body, sender).await?;
  Ok(Json(response))
}
