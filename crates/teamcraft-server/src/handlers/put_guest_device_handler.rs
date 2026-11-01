use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, GuestDeviceReadyRequest, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meeting_guests;
use crate::state::AppState;

/// A waiting guest's device is set up, with its identity key fingerprint: the hosts are told once,
/// so an organizer's device can let it in (guests only).
#[utoipa::path(put, path = "/api/v1/meetings/{meeting_id}/lobby/me/device", tag = "meetings", summary = "A waiting guest's device is set up; the hosts are told again (guests only).", params(("meeting_id" = MeetingId, Path)), request_body = GuestDeviceReadyRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_guest_device_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiJson(request): ApiJson<GuestDeviceReadyRequest>,
) -> Result<StatusCode, ApiError> {
  meeting_guests::guest_device_ready(&state, &caller, meeting_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
