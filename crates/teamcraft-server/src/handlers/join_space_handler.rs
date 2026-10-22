use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SpaceId, SpaceResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::spaces;
use crate::state::AppState;

/// Joins an open space.
#[utoipa::path(post, path = "/api/v1/spaces/{space_id}/join", tag = "spaces", summary = "Joins an open space.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = SpaceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn join_space_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<SpaceResponse>, ApiError> {
  let response = spaces::join_space(&state, &caller, space_id).await?;
  Ok(Json(response))
}
