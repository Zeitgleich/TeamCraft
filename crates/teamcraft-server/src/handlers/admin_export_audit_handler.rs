use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{AuditExportRequest, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::admin;
use crate::state::AppState;

/// Records an export of the audit log, before the client reads its pages.
#[utoipa::path(post, path = "/api/v1/admin/audit/export", tag = "admin", summary = "Records an export of the audit log, before the client reads its pages.", request_body = AuditExportRequest, responses((status = 204), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_export_audit_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<AuditExportRequest>,
) -> Result<StatusCode, ApiError> {
  admin::record_audit_export(&state, &caller, request).await?;
  Ok(StatusCode::NO_CONTENT)
}
