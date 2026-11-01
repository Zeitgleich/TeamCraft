use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::ErrorBody;

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::meeting_guests;
use crate::state::AppState;

/// The caller's app lets meeting guests in without a link secret (#987), so guests of their
/// meetings aren't told the organizer's app needs an update.
#[utoipa::path(put, path = "/api/v1/meetings/admitter", tag = "meetings", summary = "The caller's app lets meeting guests in by the current flow.", responses((status = 204), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn mark_guest_admitter_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<StatusCode, ApiError> {
  meeting_guests::mark_guest_admitter(&state, &caller).await?;
  Ok(StatusCode::NO_CONTENT)
}
