use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{ErrorBody, PutRoleEvidenceRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::role_evidence;
use crate::state::AppState;

/// Publishes the caller's newest ID token for the owners' apps (e2ee.md §3.1.3).
#[utoipa::path(put, path = "/api/v1/me/role-evidence", tag = "e2ee", summary = "Publishes the caller's newest ID token for the owners' apps.", request_body = PutRoleEvidenceRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn put_role_evidence_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<PutRoleEvidenceRequest>,
) -> Result<StatusCode, ApiError> {
  role_evidence::put(&state, &caller, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
