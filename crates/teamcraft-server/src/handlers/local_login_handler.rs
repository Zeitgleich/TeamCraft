use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, LocalLoginRequest, LocalLoginResponse};

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::local_accounts;
use crate::state::AppState;

/// Signs a local account in with password and one-time code.
#[utoipa::path(post, path = "/api/v1/auth/local/login", tag = "auth", summary = "Signs a local account in with password and one-time code.", request_body = LocalLoginRequest, responses((status = 200, body = LocalLoginResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn local_login_handler(
  State(state): State<AppState>,
  ApiJson(request): ApiJson<LocalLoginRequest>,
) -> Result<Json<LocalLoginResponse>, ApiError> {
  let response = local_accounts::login(&state, request).await?;
  Ok(Json(response))
}
