use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, RecoveryRequestId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::org_recovery;
use crate::state::AppState;

/// An admin turns the request down.
#[utoipa::path(post, path = "/api/v1/org-recovery/requests/{request_id}/deny", tag = "org recovery", summary = "An admin turns the request down.", params(("request_id" = RecoveryRequestId, Path)), responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn deny_recovery_request_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<RecoveryRequestId>,
) -> Result<StatusCode, ApiError> {
  org_recovery::deny(&state, &caller, request_id).await?;
  Ok(StatusCode::NO_CONTENT)
}
