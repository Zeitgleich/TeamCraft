use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminHealthResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::admin;
use crate::state::AppState;

/// Returns how the health checks stand and the alerts raised since the server started.
#[utoipa::path(get, path = "/api/v1/admin/health", tag = "admin", summary = "Returns how the health checks stand and the alerts raised since the server started.", responses((status = 200, body = AdminHealthResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_health_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminHealthResponse>, ApiError> {
  let response = admin::health_status(&state, &caller)?;
  Ok(Json(response))
}
