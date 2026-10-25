use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, MeetingId, PutGuestProofRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::meeting_guests;
use crate::state::AppState;

/// A guest hands the hosts the proof that they hold the link's secret (guests only, once).
#[utoipa::path(put, path = "/api/v1/meetings/{meeting_id}/lobby/me/proof", tag = "meetings", summary = "A guest hands the hosts the proof that they hold the link's secret (guests only, once).", params(("meeting_id" = MeetingId, Path)), request_body = PutGuestProofRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_guest_proof_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
  ApiJson(request): ApiJson<PutGuestProofRequest>,
) -> Result<StatusCode, ApiError> {
  meeting_guests::put_guest_proof(&state, &caller, meeting_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
