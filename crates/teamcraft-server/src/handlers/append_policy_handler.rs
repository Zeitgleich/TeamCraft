use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AppendWorkspaceLogRequest, ErrorBody, WorkspaceLogEntryResponse, WorkspaceLogKind};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::workspace;
use crate::state::AppState;

/// Appends an owner-signed workspace policy.
#[utoipa::path(post, path = "/api/v1/workspace/policy", tag = "workspace", summary = "Appends an owner-signed workspace policy.", request_body = AppendWorkspaceLogRequest, responses((status = 201, body = WorkspaceLogEntryResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn append_policy_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiJson(request): ApiJson<AppendWorkspaceLogRequest>,
) -> Result<(StatusCode, Json<WorkspaceLogEntryResponse>), ApiError> {
  let response = workspace::append(&state, &caller, WorkspaceLogKind::Policy, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
