use axum::Json;
use axum::extract::State;
use teamcraft_proto::{EmailDigestResponse, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::email_digest;
use crate::state::AppState;

/// Returns the caller's e-mail digest setting.
#[utoipa::path(get, path = "/api/v1/me/email-digest", tag = "notifications", summary = "Returns the caller's e-mail digest setting.", responses((status = 200, body = EmailDigestResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_email_digest_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<EmailDigestResponse>, ApiError> {
  let response = email_digest::get_settings(&state, &caller).await?;
  Ok(Json(response))
}
