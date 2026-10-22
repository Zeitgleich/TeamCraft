use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListAdminDevicesResponse, UserId};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin;
use crate::state::AppState;

/// Lists a user's devices including removed ones.
#[utoipa::path(get, path = "/api/v1/admin/users/{user_id}/devices", tag = "admin", summary = "Lists a user's devices including removed ones.", params(("user_id" = UserId, Path)), responses((status = 200, body = ListAdminDevicesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_user_devices_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(user_id): ApiPath<UserId>,
) -> Result<Json<ListAdminDevicesResponse>, ApiError> {
  let response = admin::list_user_devices(&state, &caller, user_id).await?;
  Ok(Json(response))
}
