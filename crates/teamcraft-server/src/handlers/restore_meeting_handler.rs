use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, MeetingId, MeetingResponse, RestoreMeetingRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meetings;
use crate::state::AppState;

/// Takes back the cancellation of a meeting or of one occurrence (organizer only).
#[utoipa::path(post, path = "/api/v1/meetings/{meeting_id}/restore", tag = "meetings", summary = "Takes back the cancellation of a meeting or of one occurrence (organizer only).", params(("meeting_id" = MeetingId, Path)), request_body = RestoreMeetingRequest, responses((status = 200, body = MeetingResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn restore_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiJson(request): ApiJson<RestoreMeetingRequest>,
) -> Result<Json<MeetingResponse>, ApiError> {
  let response = meetings::restore_meeting(&state, &caller, meeting_id, request).await?;
  Ok(Json(response))
}
