use axum::Json;
use axum::extract::State;
use teamcraft_proto::{EmailDigestResponse, ErrorBody, SetEmailDigestRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::email_digest;
use crate::state::AppState;

/// Sets the caller's e-mail digest of missed mentions.
#[utoipa::path(put, path = "/api/v1/me/email-digest", tag = "notifications", summary = "Sets the caller's e-mail digest of missed mentions.", request_body = SetEmailDigestRequest, responses((status = 200, body = EmailDigestResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 409, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_email_digest_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<SetEmailDigestRequest>,
) -> Result<Json<EmailDigestResponse>, ApiError> {
  let response = email_digest::put_settings(&state, &caller, request).await?;
  Ok(Json(response))
}
