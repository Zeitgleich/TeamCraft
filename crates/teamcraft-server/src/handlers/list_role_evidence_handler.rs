use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListRoleEvidenceResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::role_evidence;
use crate::state::AppState;

/// The role evidence and what the server knows of the roster's people, for owners' apps.
#[utoipa::path(get, path = "/api/v1/role-evidence", tag = "e2ee", summary = "The role evidence and what the server knows of the roster's people, for owners' apps.", responses((status = 200, body = ListRoleEvidenceResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_role_evidence_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListRoleEvidenceResponse>, ApiError> {
  Ok(Json(role_evidence::list(&state, &caller).await?))
}
