use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListSpaceMembersResponse, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::admin;
use crate::state::AppState;

/// Lists the members of any space.
#[utoipa::path(get, path = "/api/v1/admin/spaces/{space_id}/members", tag = "admin", summary = "Lists the members of any space.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = ListSpaceMembersResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_list_space_members_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<ListSpaceMembersResponse>, ApiError> {
  let response = admin::list_space_members(&state, &caller, space_id).await?;
  Ok(Json(response))
}
