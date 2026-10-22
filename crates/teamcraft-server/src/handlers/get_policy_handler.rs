use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, WorkspacePolicyResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::workspace;
use crate::state::AppState;

/// Returns the current workspace policy.
#[utoipa::path(get, path = "/api/v1/workspace/policy", tag = "workspace", summary = "Returns the current workspace policy.", responses((status = 200, body = WorkspacePolicyResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_policy_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<WorkspacePolicyResponse>, ApiError> {
  let response = workspace::get_policy(&state, &caller).await?;
  Ok(Json(response))
}
