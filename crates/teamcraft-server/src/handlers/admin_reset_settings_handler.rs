use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, RuntimeSettingsKey, RuntimeSettingsResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::runtime_settings;
use crate::state::AppState;

/// Removes the console's values of one group, so the configuration file's apply again.
#[utoipa::path(delete, path = "/api/v1/admin/settings/{section}", tag = "admin", summary = "Goes back to the configuration file's values of one group.", params(("section" = RuntimeSettingsKey, Path, description = "Settings group")), responses((status = 200, body = RuntimeSettingsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_reset_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(section): ApiPath<RuntimeSettingsKey>,
) -> Result<Json<RuntimeSettingsResponse>, ApiError> {
  let response = runtime_settings::reset_settings(&state, &caller, section).await?;
  Ok(Json(response))
}
