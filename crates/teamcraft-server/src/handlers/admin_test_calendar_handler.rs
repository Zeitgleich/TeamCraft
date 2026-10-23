use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SettingsTestResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::calendar_settings;
use crate::state::AppState;

/// Asks the calendar server whether the calendar can be reached with the account.
#[utoipa::path(post, path = "/api/v1/admin/settings/calendar/test", tag = "admin", summary = "Tests the connection to the calendar in force.", responses((status = 200, body = SettingsTestResponse), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_test_calendar_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<SettingsTestResponse>, ApiError> {
  let response = calendar_settings::test_calendar(&state, &caller).await?;
  Ok(Json(response))
}
