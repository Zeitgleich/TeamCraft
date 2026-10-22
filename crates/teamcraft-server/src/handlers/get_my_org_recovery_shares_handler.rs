use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, OrgRecoverySharesResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::org_recovery;
use crate::state::AppState;

/// Returns the shares sealed to the caller's devices.
#[utoipa::path(get, path = "/api/v1/org-recovery/shares/mine", tag = "org recovery", summary = "Returns the shares sealed to the caller's devices.", responses((status = 200, body = OrgRecoverySharesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_my_org_recovery_shares_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<OrgRecoverySharesResponse>, ApiError> {
  let response = org_recovery::my_shares(&state, &caller).await?;
  Ok(Json(response))
}
