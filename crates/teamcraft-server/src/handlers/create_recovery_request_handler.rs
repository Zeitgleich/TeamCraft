use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CreateRecoveryRequest, ErrorBody, RecoveryRequestResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::org_recovery;
use crate::state::AppState;

/// A new device asks for recovery with the org key.
#[utoipa::path(post, path = "/api/v1/org-recovery/requests", tag = "org recovery", summary = "A new device asks for recovery with the org key.", request_body = CreateRecoveryRequest, responses((status = 201, body = RecoveryRequestResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_recovery_request_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<CreateRecoveryRequest>,
) -> Result<(StatusCode, Json<RecoveryRequestResponse>), ApiError> {
  let response = org_recovery::create_request(&state, &caller, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
