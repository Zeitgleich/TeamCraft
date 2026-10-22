use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PermissionSchemeResponse, SpaceId, UpdatePermissionSchemeRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::permission_scheme;
use crate::state::AppState;

/// Changes the permission scheme of a space.
#[utoipa::path(put, path = "/api/v1/admin/spaces/{space_id}/permission-scheme", tag = "admin", summary = "Changes the permission scheme of a space.", request_body = UpdatePermissionSchemeRequest, params(("space_id" = SpaceId, Path)), responses((status = 200, body = PermissionSchemeResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_update_space_permission_scheme_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<UpdatePermissionSchemeRequest>,
) -> Result<Json<PermissionSchemeResponse>, ApiError> {
  let response = permission_scheme::update_space_scheme(&state, &caller, space_id, request).await?;
  Ok(Json(response))
}
