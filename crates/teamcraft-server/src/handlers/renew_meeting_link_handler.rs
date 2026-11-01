use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, MeetingId, MeetingResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meetings;
use crate::state::AppState;

/// Makes the meeting's link anew (organizer only): the old link opens nothing any more, and the
/// invitees get updated invitations with the new one.
#[utoipa::path(post, path = "/api/v1/meetings/{meeting_id}/link", tag = "meetings", summary = "Makes the meeting's link anew; the old one stops working and invitees get the new one (organizer only).", params(("meeting_id" = MeetingId, Path)), responses((status = 200, body = MeetingResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn renew_meeting_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
) -> Result<Json<MeetingResponse>, ApiError> {
  let response = meetings::renew_link(&state, &caller, meeting_id).await?;
  Ok(Json(response))
}
