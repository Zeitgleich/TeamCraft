use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListInvitesResponse, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::invites;
use crate::state::AppState;

/// Lists pending invites of a space.
#[utoipa::path(get, path = "/api/v1/spaces/{space_id}/invites", tag = "invites", summary = "Lists pending invites of a space.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = ListInvitesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_invites_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<ListInvitesResponse>, ApiError> {
  let response = invites::list_invites(&state, &caller, space_id).await?;
  Ok(Json(response))
}
