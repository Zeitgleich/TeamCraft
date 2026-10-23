use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminCalendarSettingsResponse, ErrorBody, UpdateCalendarSettingsRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::calendar_settings;
use crate::state::AppState;

/// Replaces the CalDAV calendar; the password is write-only and stored encrypted.
#[utoipa::path(put, path = "/api/v1/admin/settings/calendar", tag = "admin", summary = "Replaces the calendar settings.", request_body = UpdateCalendarSettingsRequest, responses((status = 200, body = AdminCalendarSettingsResponse), (status = 400, body = ErrorBody), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_put_calendar_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<UpdateCalendarSettingsRequest>,
) -> Result<Json<AdminCalendarSettingsResponse>, ApiError> {
  let response = calendar_settings::put_calendar_settings(&state, &caller, request).await?;
  Ok(Json(response))
}
