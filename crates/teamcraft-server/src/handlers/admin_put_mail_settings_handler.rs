use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminMailSettingsResponse, ErrorBody, UpdateMailSettingsRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::mail_settings;
use crate::state::AppState;

/// Replaces the mail server; the password is write-only and stored encrypted.
#[utoipa::path(put, path = "/api/v1/admin/settings/mail", tag = "admin", summary = "Replaces the mail server settings.", request_body = UpdateMailSettingsRequest, responses((status = 200, body = AdminMailSettingsResponse), (status = 400, body = ErrorBody), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_put_mail_settings_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<UpdateMailSettingsRequest>,
) -> Result<Json<AdminMailSettingsResponse>, ApiError> {
  let response = mail_settings::put_mail_settings(&state, &caller, request).await?;
  Ok(Json(response))
}
