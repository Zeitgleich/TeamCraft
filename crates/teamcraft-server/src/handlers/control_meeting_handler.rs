use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, MeetingControlRequest, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meeting_controls;
use crate::state::AppState;

/// Host controls: mute all, remove a participant, lock, unlock, end for everyone.
#[utoipa::path(post, path = "/api/v1/meetings/{meeting_id}/controls", tag = "meetings", summary = "Host controls: mute all, remove a participant, lock, unlock, end for everyone.", request_body = MeetingControlRequest, params(("meeting_id" = MeetingId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn control_meeting_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiJson(request): ApiJson<MeetingControlRequest>,
) -> Result<StatusCode, ApiError> {
  meeting_controls::control_meeting(&state, &caller, meeting_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
