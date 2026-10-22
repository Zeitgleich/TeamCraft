use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, RecoveryRequestId, RecoveryRequestResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::org_recovery;
use crate::state::AppState;

/// Returns one recovery request (admins and the person).
#[utoipa::path(get, path = "/api/v1/org-recovery/requests/{request_id}", tag = "org recovery", summary = "Returns one recovery request (admins and the person).", params(("request_id" = RecoveryRequestId, Path)), responses((status = 200, body = RecoveryRequestResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_recovery_request_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(request_id): ApiPath<RecoveryRequestId>,
) -> Result<Json<RecoveryRequestResponse>, ApiError> {
  let response = org_recovery::get_request(&state, &caller, request_id).await?;
  Ok(Json(response))
}
