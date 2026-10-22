use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AnnouncementId, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::announcements;
use crate::state::AppState;

/// Hides the banner for the caller.
#[utoipa::path(post, path = "/api/v1/announcement/{announcement_id}/dismiss", tag = "announcements", summary = "Hides the banner for the caller.", params(("announcement_id" = AnnouncementId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn dismiss_announcement_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(announcement_id): ApiPath<AnnouncementId>,
) -> Result<StatusCode, ApiError> {
  announcements::dismiss_announcement(&state, &caller, announcement_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
