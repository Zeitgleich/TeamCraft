use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SendTestMailRequest, SettingsTestResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::mail_settings;
use crate::state::AppState;

/// Sends a test mail to the caller or the given address and reports the outcome.
#[utoipa::path(post, path = "/api/v1/admin/settings/mail/test", tag = "admin", summary = "Sends a test mail through the mail server in force.", request_body = SendTestMailRequest, responses((status = 200, body = SettingsTestResponse), (status = 400, body = ErrorBody), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_send_test_mail_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<SendTestMailRequest>,
) -> Result<Json<SettingsTestResponse>, ApiError> {
  let response = mail_settings::send_test_mail(&state, &caller, request).await?;
  Ok(Json(response))
}
