use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, RuntimeSettingsResponse, RuntimeSettingsSection};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::runtime_settings;
use crate::state::AppState;

/// Replaces one group of instance settings.
#[utoipa::path(put, path = "/api/v1/admin/settings", tag = "admin", summary = "Replaces one group of instance settings.", request_body = RuntimeSettingsSection, responses((status = 200, body = RuntimeSettingsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_put_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<RuntimeSettingsSection>,
) -> Result<Json<RuntimeSettingsResponse>, ApiError> {
  let response = runtime_settings::put_settings(&state, &caller, request).await?;
  Ok(Json(response))
}
