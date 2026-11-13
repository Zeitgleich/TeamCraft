use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SpaceDeletionPreviewResponse, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::spaces;
use crate::state::AppState;

/// What deleting a space takes, for the deleter's confirmation and signature.
#[utoipa::path(get, path = "/api/v1/spaces/{space_id}/deletion", tag = "spaces", summary = "What deleting a space takes.", description = "Needs `delete_space`. The space's channels the caller may see (public ones, those they are in; all for an instance admin), with their names and their groups' epochs now: what a workspace admin's device signs for `DELETE /spaces/{id}` (e2ee.md §4.6.5, D7). The server's word; members' devices check it against each channel's anchor.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = SpaceDeletionPreviewResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn space_deletion_preview_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<SpaceDeletionPreviewResponse>, ApiError> {
  let response = spaces::space_deletion_preview(&state, &caller, space_id).await?;
  Ok(Json(response))
}
