use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, RecoveryRequestId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::org_recovery;
use crate::state::AppState;

/// The person withdraws the request.
#[utoipa::path(delete, path = "/api/v1/org-recovery/requests/{request_id}", tag = "org recovery", summary = "The person withdraws the request.", params(("request_id" = RecoveryRequestId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn cancel_recovery_request_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<RecoveryRequestId>,
) -> Result<StatusCode, ApiError> {
  org_recovery::cancel(&state, &caller, request_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
