use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{DismissMeetingNoticeRequest, ErrorBody, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meeting_notices;
use crate::state::AppState;

/// Reports that the caller dealt with a meeting's reminder or start notice on a device.
#[utoipa::path(post, path = "/api/v1/meetings/{meeting_id}/notices/dismiss", tag = "notifications", summary = "Reports a meeting notice as dealt with, so the caller's other phones take it down.", request_body = DismissMeetingNoticeRequest, params(("meeting_id" = MeetingId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 429, body = ErrorBody)), security(("bearer" = [])))]
pub async fn dismiss_meeting_notice_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiJson(request): ApiJson<DismissMeetingNoticeRequest>,
) -> Result<StatusCode, ApiError> {
  meeting_notices::dismiss(&state, &caller, meeting_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
