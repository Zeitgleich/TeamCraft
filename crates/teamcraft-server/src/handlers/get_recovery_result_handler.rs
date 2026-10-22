use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, RecoveryRequestId, RecoveryResultPayload};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::org_recovery;
use crate::state::AppState;

/// The recovering device fetches the sealed backup key.
#[utoipa::path(get, path = "/api/v1/org-recovery/requests/{request_id}/result", tag = "org recovery", summary = "The recovering device fetches the sealed backup key.", params(("request_id" = RecoveryRequestId, Path)), responses((status = 200, body = RecoveryResultPayload), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_recovery_result_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<RecoveryRequestId>,
) -> Result<Json<RecoveryResultPayload>, ApiError> {
  let response = org_recovery::get_result(&state, &caller, request_id).await?;
  Ok(Json(response))
}
