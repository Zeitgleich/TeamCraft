use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateMeetingRequest, ErrorBody, MeetingResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::meetings;
use crate::state::AppState;

/// Schedules a meeting: chat, calendar event and invitations.
#[utoipa::path(post, path = "/api/v1/meetings", tag = "meetings", summary = "Schedules a meeting: chat, calendar event and invitations.", request_body = CreateMeetingRequest, responses((status = 201, body = MeetingResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateMeetingRequest>,
) -> Result<(StatusCode, Json<MeetingResponse>), ApiError> {
  let response = meetings::create_meeting(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
