use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListWorkspaceLogQuery, ListWorkspaceLogResponse, WorkspaceLogKind};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::workspace;
use crate::state::AppState;

/// Returns roster entries after a version.
#[utoipa::path(get, path = "/api/v1/workspace/roster", tag = "workspace", summary = "Returns roster entries after a version.", params(ListWorkspaceLogQuery), responses((status = 200, body = ListWorkspaceLogResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_roster_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<ListWorkspaceLogQuery>,
) -> Result<Json<ListWorkspaceLogResponse>, ApiError> {
  let response = workspace::list(&state, &caller, WorkspaceLogKind::Roster, query).await?;
  Ok(Json(response))
}
