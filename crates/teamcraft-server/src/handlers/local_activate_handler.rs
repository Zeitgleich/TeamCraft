use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ActivateLocalAccountRequest, ActivateLocalAccountResponse, ErrorBody};

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::local_accounts;
use crate::state::AppState;

/// Spends an activation token and sets the password of a local account.
#[utoipa::path(post, path = "/api/v1/auth/local/activate", tag = "auth", summary = "Spends an activation token and sets the password of a local account.", request_body = ActivateLocalAccountRequest, responses((status = 200, body = ActivateLocalAccountResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)))]
pub async fn local_activate_handler(
  State(state): State<AppState>,
  ApiJson(request): ApiJson<ActivateLocalAccountRequest>,
) -> Result<Json<ActivateLocalAccountResponse>, ApiError> {
  let response = local_accounts::activate(&state, request).await?;
  Ok(Json(response))
}
