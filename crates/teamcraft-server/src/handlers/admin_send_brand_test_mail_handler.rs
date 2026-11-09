use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SendTestMailRequest, SettingsTestResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::brand_mail;
use crate::state::AppState;

/// Sends a test mail through a brand's own mail server and reports the outcome.
#[utoipa::path(post, path = "/api/v1/admin/brands/{brand_id}/mail-server/test", tag = "admin", summary = "Sends a test mail through a brand's own mail server.", params(("brand_id" = String, Path, description = "Brand id")), request_body = SendTestMailRequest, responses((status = 200, body = SettingsTestResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_send_brand_test_mail_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(brand_id): ApiPath<String>,
  ApiJson(request): ApiJson<SendTestMailRequest>,
) -> Result<Json<SettingsTestResponse>, ApiError> {
  let response = brand_mail::send_test_mail(&state, &caller, &brand_id, request).await?;
  Ok(Json(response))
}
