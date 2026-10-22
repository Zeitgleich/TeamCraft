use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, PutRecoveryShareRequest, RecoveryRequestId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::org_recovery;
use crate::state::AppState;

/// An approver hands in their share sealed to the combining admin.
#[utoipa::path(put, path = "/api/v1/org-recovery/requests/{request_id}/share", tag = "org recovery", summary = "An approver hands in their share sealed to the combining admin.", request_body = PutRecoveryShareRequest, params(("request_id" = RecoveryRequestId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_recovery_share_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<RecoveryRequestId>,
  ApiJson(request): ApiJson<PutRecoveryShareRequest>,
) -> Result<StatusCode, ApiError> {
  org_recovery::put_share(&state, &caller, request_id, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
