use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_guests;
use crate::state::AppState;

/// Revokes the meeting's shared guest link (host only).
#[utoipa::path(delete, path = "/api/v1/meetings/{meeting_id}/shared-guest-link", tag = "meetings", summary = "Revokes the meeting's shared guest link (host only).", params(("meeting_id" = MeetingId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn revoke_shared_guest_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
) -> Result<StatusCode, ApiError> {
  meeting_guests::revoke_shared_guest_link(&state, &caller, meeting_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
