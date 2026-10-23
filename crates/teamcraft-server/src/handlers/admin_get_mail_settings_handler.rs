use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminMailSettingsResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::mail_settings;
use crate::state::AppState;

/// Returns the mail server in force, where it comes from and how the last mail went.
#[utoipa::path(get, path = "/api/v1/admin/settings/mail", tag = "admin", summary = "Returns the mail server settings without the password.", responses((status = 200, body = AdminMailSettingsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_mail_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminMailSettingsResponse>, ApiError> {
  let response = mail_settings::get_mail_settings(&state, &caller).await?;
  Ok(Json(response))
}
