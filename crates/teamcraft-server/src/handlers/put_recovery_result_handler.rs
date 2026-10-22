use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, RecoveryRequestId, RecoveryResultPayload};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::org_recovery;
use crate::state::AppState;

/// The combining admin hands over the sealed backup key.
#[utoipa::path(put, path = "/api/v1/org-recovery/requests/{request_id}/result", tag = "org recovery", summary = "The combining admin hands over the sealed backup key.", request_body = RecoveryResultPayload, params(("request_id" = RecoveryRequestId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_recovery_result_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<RecoveryRequestId>,
  ApiJson(request): ApiJson<RecoveryResultPayload>,
) -> Result<StatusCode, ApiError> {
  org_recovery::put_result(&state, &caller, request_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
