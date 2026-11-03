use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_guests;
use crate::state::AppState;

/// A guest leaves the meeting; their session ends (guests only).
#[utoipa::path(post, path = "/api/v1/meetings/{meeting_id}/leave", tag = "meetings", summary = "A guest leaves the meeting; their session ends (guests only).", params(("meeting_id" = MeetingId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn leave_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
) -> Result<StatusCode, ApiError> {
  meeting_guests::leave(&state, &caller, meeting_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
