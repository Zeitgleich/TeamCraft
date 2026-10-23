use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminCalendarSettingsResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::calendar_settings;
use crate::state::AppState;

/// Removes the console's calendar, so the configuration file's applies again.
#[utoipa::path(delete, path = "/api/v1/admin/settings/calendar", tag = "admin", summary = "Goes back to the calendar of the configuration file.", responses((status = 200, body = AdminCalendarSettingsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_reset_calendar_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminCalendarSettingsResponse>, ApiError> {
  let response = calendar_settings::reset_calendar_settings(&state, &caller).await?;
  Ok(Json(response))
}
