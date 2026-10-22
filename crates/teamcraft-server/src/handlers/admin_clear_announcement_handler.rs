use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::ErrorBody;

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::announcements;
use crate::state::AppState;

/// Ends the announcement banner.
#[utoipa::path(delete, path = "/api/v1/admin/announcement", tag = "admin", summary = "Ends the announcement banner.", responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_clear_announcement_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<StatusCode, ApiError> {
  announcements::clear_announcement(&state, &caller).await?;
  Ok(StatusCode::NO_CONTENT)
}
