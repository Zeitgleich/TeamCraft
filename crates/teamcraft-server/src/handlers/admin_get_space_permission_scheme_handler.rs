use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, PermissionSchemeResponse, SpaceId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::permission_scheme;
use crate::state::AppState;

/// Returns the permission scheme of a space.
#[utoipa::path(get, path = "/api/v1/admin/spaces/{space_id}/permission-scheme", tag = "admin", summary = "Returns the permission scheme of a space.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = PermissionSchemeResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_space_permission_scheme_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<PermissionSchemeResponse>, ApiError> {
  let response = permission_scheme::get_space_scheme(&state, &caller, space_id).await?;
  Ok(Json(response))
}
