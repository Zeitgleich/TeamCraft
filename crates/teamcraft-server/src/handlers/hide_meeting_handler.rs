use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meetings;
use crate::state::AppState;

/// Takes a cancelled meeting off everybody's meeting lists (organizer only); its chat stays.
#[utoipa::path(post, path = "/api/v1/meetings/{meeting_id}/hide", tag = "meetings", summary = "Takes a cancelled meeting off the meeting lists (organizer only).", params(("meeting_id" = MeetingId, Path)), responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn hide_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
) -> Result<StatusCode, ApiError> {
  meetings::hide_meeting(&state, &caller, meeting_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
