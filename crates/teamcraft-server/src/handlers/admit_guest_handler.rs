use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, MeetingId, UserId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::meeting_guests;
use crate::state::AppState;

/// Lets a waiting guest into the meeting (host only).
#[utoipa::path(post, path = "/api/v1/meetings/{meeting_id}/lobby/{user_id}/admit", tag = "meetings", summary = "Lets a waiting guest into the meeting (host only).", params(("meeting_id" = MeetingId, Path), ("user_id" = UserId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admit_guest_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((meeting_id, user_id)): ApiPath<(MeetingId, UserId)>,
) -> Result<StatusCode, ApiError> {
  meeting_guests::admit(&state, &caller, meeting_id, user_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
