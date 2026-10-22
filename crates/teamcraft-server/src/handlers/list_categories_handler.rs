use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, ListCategoriesResponse, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::spaces;
use crate::state::AppState;

/// Lists the channel categories of a space.
#[utoipa::path(get, path = "/api/v1/spaces/{space_id}/categories", tag = "spaces", summary = "Lists the channel categories of a space.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = ListCategoriesResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn list_categories_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<ListCategoriesResponse>, ApiError> {
  let response = spaces::list_categories(&state, &caller, space_id).await?;
  Ok(Json(response))
}
