use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateInstantMeetingRequest, ErrorBody, MeetingResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::meetings;
use crate::state::AppState;

/// Starts a meeting right away (Meet now).
#[utoipa::path(post, path = "/api/v1/meetings/instant", tag = "meetings", summary = "Starts a meeting right away (Meet now).", request_body = CreateInstantMeetingRequest, responses((status = 201, body = MeetingResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_instant_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateInstantMeetingRequest>,
) -> Result<(StatusCode, Json<MeetingResponse>), ApiError> {
  let response = meetings::create_instant_meeting(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
