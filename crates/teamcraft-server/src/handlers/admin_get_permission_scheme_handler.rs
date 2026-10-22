use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PermissionSchemeResponse};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::services::permission_scheme;
use crate::state::AppState;

/// Returns the instance permission scheme.
#[utoipa::path(get, path = "/api/v1/admin/permission-scheme", tag = "admin", summary = "Returns the instance permission scheme.", responses((status = 200, body = PermissionSchemeResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_permission_scheme_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
) -> Result<Json<PermissionSchemeResponse>, ApiError> {
  let response = permission_scheme::get_instance_scheme(&state, &caller).await?;
  Ok(Json(response))
}
