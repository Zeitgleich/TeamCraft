use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ApproveRecoveryRequest, ErrorBody, RecoveryRequestId, RecoveryRequestResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::org_recovery;
use crate::state::AppState;

/// An admin approves on one of their devices.
#[utoipa::path(post, path = "/api/v1/org-recovery/requests/{request_id}/approve", tag = "org recovery", summary = "An admin approves on one of their devices.", request_body = ApproveRecoveryRequest, params(("request_id" = RecoveryRequestId, Path)), responses((status = 200, body = RecoveryRequestResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn approve_recovery_request_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<RecoveryRequestId>,
  ApiJson(request): ApiJson<ApproveRecoveryRequest>,
) -> Result<Json<RecoveryRequestResponse>, ApiError> {
  let response = org_recovery::approve(&state, &caller, request_id, request).await?;
  Ok(Json(response))
}
