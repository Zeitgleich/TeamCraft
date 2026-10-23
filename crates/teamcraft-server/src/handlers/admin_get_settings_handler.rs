use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, RuntimeSettingsResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::runtime_settings;
use crate::state::AppState;

/// Returns the instance settings in force and whether each group comes from the configuration file or the console.
#[utoipa::path(get, path = "/api/v1/admin/settings", tag = "admin", summary = "Returns the instance settings and where each group comes from.", responses((status = 200, body = RuntimeSettingsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<RuntimeSettingsResponse>, ApiError> {
  let response = runtime_settings::get_settings(&state, &caller).await?;
  Ok(Json(response))
}
