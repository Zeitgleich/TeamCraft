use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SpaceId, SpaceResponse};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::spaces;
use crate::state::AppState;

/// Returns one space.
#[utoipa::path(get, path = "/api/v1/spaces/{space_id}", tag = "spaces", summary = "Returns one space.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = SpaceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_space_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<SpaceResponse>, ApiError> {
  let response = spaces::get_space(&state, &caller, space_id).await?;
  Ok(Json(response))
}
