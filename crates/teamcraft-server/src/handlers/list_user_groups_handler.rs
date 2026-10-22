use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListUserGroupsResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::user_groups;
use crate::state::AppState;

/// Lists the user groups.
#[utoipa::path(get, path = "/api/v1/user-groups", tag = "user groups", summary = "Lists the user groups.", responses((status = 200, body = ListUserGroupsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_user_groups_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListUserGroupsResponse>, ApiError> {
  let response = user_groups::list_user_groups(&state, &caller).await?;
  Ok(Json(response))
}
