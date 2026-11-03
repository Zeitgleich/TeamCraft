use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CancelMeetingQuery, ErrorBody, MeetingId, MeetingResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiPath, ApiQuery};
use crate::services::meetings;
use crate::state::AppState;

/// Cancels a meeting, one occurrence of a repeating one, or an occurrence and all after it
/// (organizer only); the chat stays readable.
#[utoipa::path(delete, path = "/api/v1/meetings/{meeting_id}", tag = "meetings", summary = "Cancels a meeting, one occurrence, or an occurrence and all following (organizer only).", params(("meeting_id" = MeetingId, Path), CancelMeetingQuery), responses((status = 200, body = MeetingResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn cancel_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiQuery(query): ApiQuery<CancelMeetingQuery>,
) -> Result<Json<MeetingResponse>, ApiError> {
  let response = meetings::cancel_meeting(&state, &caller, meeting_id, query).await?;
  Ok(Json(response))
}
