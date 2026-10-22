use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, MeetingId, MeetingResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meetings;
use crate::state::AppState;

/// Returns one meeting.
#[utoipa::path(get, path = "/api/v1/meetings/{meeting_id}", tag = "meetings", summary = "Returns one meeting.", params(("meeting_id" = MeetingId, Path)), responses((status = 200, body = MeetingResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
) -> Result<Json<MeetingResponse>, ApiError> {
  let response = meetings::get_meeting(&state, &caller, meeting_id).await?;
  Ok(Json(response))
}
