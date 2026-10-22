use axum::Json;
use axum::extract::State;
use teamcraft_proto::{ErrorBody, SpaceId, SpaceResponse, UpdateSpaceRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::spaces;
use crate::state::AppState;

/// Changes a space's properties or archives it.
#[utoipa::path(patch, path = "/api/v1/spaces/{space_id}", tag = "spaces", summary = "Changes a space's properties or archives it.", request_body = UpdateSpaceRequest, params(("space_id" = SpaceId, Path)), responses((status = 200, body = SpaceResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_space_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<UpdateSpaceRequest>,
) -> Result<Json<SpaceResponse>, ApiError> {
  let response = spaces::update_space(&state, &caller, space_id, request).await?;
  Ok(Json(response))
}
