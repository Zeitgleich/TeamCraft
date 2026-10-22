use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, MeetingId, MeetingResponse, UpdateMeetingRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meetings;
use crate::state::AppState;

/// Changes a meeting (organizer only).
#[utoipa::path(patch, path = "/api/v1/meetings/{meeting_id}", tag = "meetings", summary = "Changes a meeting (organizer only).", request_body = UpdateMeetingRequest, params(("meeting_id" = MeetingId, Path)), responses((status = 200, body = MeetingResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiJson(request): ApiJson<UpdateMeetingRequest>,
) -> Result<Json<MeetingResponse>, ApiError> {
  let response = meetings::update_meeting(&state, &caller, meeting_id, request).await?;
  Ok(Json(response))
}
