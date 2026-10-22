use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, GetAnnouncementResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::announcements;
use crate::state::AppState;

/// Returns the announcement banner in force.
#[utoipa::path(get, path = "/api/v1/announcement", tag = "announcements", summary = "Returns the announcement banner in force.", responses((status = 200, body = GetAnnouncementResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_announcement_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<GetAnnouncementResponse>, ApiError> {
  let response = announcements::get_announcement(&state, &caller).await?;
  Ok(Json(response))
}
