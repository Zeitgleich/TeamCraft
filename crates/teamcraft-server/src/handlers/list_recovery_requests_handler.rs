use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListRecoveryRequestsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::org_recovery;
use crate::state::AppState;

/// Lists the open recovery requests (admins).
#[utoipa::path(get, path = "/api/v1/org-recovery/requests", tag = "org recovery", summary = "Lists the open recovery requests (admins).", responses((status = 200, body = ListRecoveryRequestsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_recovery_requests_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListRecoveryRequestsResponse>, ApiError> {
  let response = org_recovery::list_requests(&state, &caller).await?;
  Ok(Json(response))
}
