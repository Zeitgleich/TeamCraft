use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, GuestLinkResponse, MeetingId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_guests;
use crate::state::AppState;

/// Returns the meeting's shared guest link, made on first use (host only).
#[utoipa::path(put, path = "/api/v1/meetings/{meeting_id}/shared-guest-link", tag = "meetings", summary = "Returns the meeting's shared guest link, made on first use (host only).", params(("meeting_id" = MeetingId, Path)), responses((status = 200, body = GuestLinkResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_shared_guest_link_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(meeting_id): ApiPath<MeetingId>,
) -> Result<Json<GuestLinkResponse>, ApiError> {
  let response = meeting_guests::shared_guest_link(&state, &caller, meeting_id).await?;
  Ok(Json(response))
}
