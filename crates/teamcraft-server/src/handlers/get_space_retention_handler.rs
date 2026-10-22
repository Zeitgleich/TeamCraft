use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, RetentionStatusResponse, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::retention;
use crate::state::AppState;

/// Returns a space's retention and the workspace limit.
#[utoipa::path(get, path = "/api/v1/spaces/{space_id}/retention", tag = "retention", summary = "Returns a space's retention and the workspace limit.", params(("space_id" = SpaceId, Path)), responses((status = 200, body = RetentionStatusResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn get_space_retention_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
) -> Result<Json<RetentionStatusResponse>, ApiError> {
  let response = retention::get_space_retention(&state, &caller, space_id).await?;
  Ok(Json(response))
}
