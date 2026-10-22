use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListAuditQuery, ListAuditResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::admin;
use crate::state::AppState;

/// Returns the audit log, newest first, with filters.
#[utoipa::path(get, path = "/api/v1/admin/audit", tag = "admin", summary = "Returns the audit log, newest first, with filters.", params(ListAuditQuery), responses((status = 200, body = ListAuditResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_audit_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiQuery(query): ApiQuery<ListAuditQuery>,
) -> Result<Json<ListAuditResponse>, ApiError> {
  let response = admin::list_audit(&state, &caller, query).await?;
  Ok(Json(response))
}
