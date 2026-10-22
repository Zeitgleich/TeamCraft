use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListAdminUsersQuery, ListAdminUsersResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiQuery;
use crate::services::admin;
use crate::state::AppState;

/// Lists users with status, devices and last seen.
#[utoipa::path(get, path = "/api/v1/admin/users", tag = "admin", summary = "Lists users with status, devices and last seen.", params(ListAdminUsersQuery), responses((status = 200, body = ListAdminUsersResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_users_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiQuery(query): ApiQuery<ListAdminUsersQuery>,
) -> Result<Json<ListAdminUsersResponse>, ApiError> {
  let response = admin::list_users(&state, &caller, query).await?;
  Ok(Json(response))
}
