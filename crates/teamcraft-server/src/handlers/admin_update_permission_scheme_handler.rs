use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PermissionSchemeResponse, UpdatePermissionSchemeRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::permission_scheme;
use crate::state::AppState;

/// Changes the instance permission scheme.
#[utoipa::path(put, path = "/api/v1/admin/permission-scheme", tag = "admin", summary = "Changes the instance permission scheme.", request_body = UpdatePermissionSchemeRequest, responses((status = 200, body = PermissionSchemeResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_update_permission_scheme_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<UpdatePermissionSchemeRequest>,
) -> Result<Json<PermissionSchemeResponse>, ApiError> {
  let response = permission_scheme::update_instance_scheme(&state, &caller, request).await?;
  Ok(Json(response))
}
