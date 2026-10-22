use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminInstanceResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::admin;
use crate::state::AppState;

/// Returns the media server, push provider and storage status of the instance.
#[utoipa::path(get, path = "/api/v1/admin/instance", tag = "admin", summary = "Returns the media server, push provider and storage status of the instance.", responses((status = 200, body = AdminInstanceResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_instance_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<AdminInstanceResponse>, ApiError> {
  let response = admin::instance_status(&state, &caller).await?;
  Ok(Json(response))
}
