use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminMailSettingsResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::mail_settings;
use crate::state::AppState;

/// Removes the console's mail server, so the configuration file's applies again.
#[utoipa::path(delete, path = "/api/v1/admin/settings/mail", tag = "admin", summary = "Goes back to the mail server of the configuration file.", responses((status = 200, body = AdminMailSettingsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_reset_mail_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminMailSettingsResponse>, ApiError> {
  let response = mail_settings::reset_mail_settings(&state, &caller).await?;
  Ok(Json(response))
}
