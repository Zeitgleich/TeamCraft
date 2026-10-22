use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, PutOrgRecoverySharesRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::org_recovery;
use crate::state::AppState;

/// Stores the sealed shares of the org secret (owners).
#[utoipa::path(put, path = "/api/v1/org-recovery/shares", tag = "org recovery", summary = "Stores the sealed shares of the org secret (owners).", request_body = PutOrgRecoverySharesRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_org_recovery_shares_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<PutOrgRecoverySharesRequest>,
) -> Result<StatusCode, ApiError> {
  org_recovery::put_shares(&state, &caller, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
