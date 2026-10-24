use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SpaceId, SpaceResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::space_images;
use crate::state::AppState;

/// Removes the picture of a space (space admins).
#[utoipa::path(delete, path = "/api/v1/spaces/{space_id}/image", tag = "spaces", summary = "Removes the picture of a space.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = SpaceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn delete_space_image_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<SpaceResponse>, ApiError> {
  let response = space_images::delete_space_image(&state, &caller, space_id).await?;
  Ok(Json(response))
}
