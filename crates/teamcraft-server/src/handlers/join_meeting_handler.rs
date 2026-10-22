use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, JoinMeetingRequest, JoinMeetingResponse};

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::meeting_guests;
use crate::state::AppState;

/// Trades a personal join link for a guest session; no authentication, the link is the credential.
#[utoipa::path(post, path = "/api/v1/meetings/join", tag = "meetings", summary = "Trades a personal join link for a guest session (no authentication).", request_body = JoinMeetingRequest, responses((status = 200, body = JoinMeetingResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 503, body = ErrorBody)))]
pub async fn join_meeting_handler(
  State(state): State<AppState>,
  ApiJson(request): ApiJson<JoinMeetingRequest>,
) -> Result<Json<JoinMeetingResponse>, ApiError> {
  let response = meeting_guests::join_meeting(&state, request).await?;
  Ok(Json(response))
}
