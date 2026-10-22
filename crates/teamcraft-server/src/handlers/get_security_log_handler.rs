use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListAuditQuery, ListAuditResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::admin;
use crate::state::AppState;

/// Returns what happened to the caller's own account.
#[utoipa::path(get, path = "/api/v1/me/security-log", tag = "admin", summary = "Returns what happened to the caller's own account.", params(ListAuditQuery), responses((status = 200, body = ListAuditResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_security_log_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<ListAuditQuery>,
) -> Result<Json<ListAuditResponse>, ApiError> {
  let response = admin::security_log(&state, &caller, query).await?;
  Ok(Json(response))
}
