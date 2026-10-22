use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ChangeLocalPasswordRequest, ErrorBody};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::local_accounts;
use crate::state::AppState;

/// Changes the password of a local account and ends its sessions.
#[utoipa::path(post, path = "/api/v1/auth/local/password", tag = "auth", summary = "Changes the password of a local account and ends its sessions.", request_body = ChangeLocalPasswordRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn change_local_password_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<ChangeLocalPasswordRequest>,
) -> Result<StatusCode, ApiError> {
  local_accounts::change_password(&state, &caller, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
