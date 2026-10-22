use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, MeetingId, SetRsvpRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meetings;
use crate::state::AppState;

/// Answers an invitation.
#[utoipa::path(put, path = "/api/v1/meetings/{meeting_id}/rsvp", tag = "meetings", summary = "Answers an invitation.", request_body = SetRsvpRequest, params(("meeting_id" = MeetingId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn set_meeting_rsvp_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiJson(request): ApiJson<SetRsvpRequest>,
) -> Result<StatusCode, ApiError> {
  meetings::set_rsvp(&state, &caller, meeting_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
