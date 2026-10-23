use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminCalendarSettingsResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::calendar_settings;
use crate::state::AppState;

/// Returns the CalDAV calendar meetings are written to and where it comes from.
#[utoipa::path(get, path = "/api/v1/admin/settings/calendar", tag = "admin", summary = "Returns the calendar settings without the password.", responses((status = 200, body = AdminCalendarSettingsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_calendar_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminCalendarSettingsResponse>, ApiError> {
  let response = calendar_settings::get_calendar_settings(&state, &caller).await?;
  Ok(Json(response))
}
