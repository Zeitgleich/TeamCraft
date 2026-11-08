use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListUserGroupRostersResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::services::user_groups;
use crate::state::AppState;

/// Lists every user group with its active members (for expanding group mentions on the device).
#[utoipa::path(get, path = "/api/v1/user-groups/rosters", tag = "user groups", summary = "Lists every user group with its active members.", responses((status = 200, body = ListUserGroupRostersResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_user_group_rosters_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
) -> Result<Json<ListUserGroupRostersResponse>, ApiError> {
  let response = user_groups::list_user_group_rosters(&state, &caller).await?;
  Ok(Json(response))
}
