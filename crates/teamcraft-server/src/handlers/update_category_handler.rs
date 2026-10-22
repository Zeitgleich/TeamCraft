use axum::Json;
use axum::extract::State;
use teamcraft_proto::{CategoryId, CategoryResponse, ErrorBody, SpaceId, UpdateCategoryRequest};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::spaces;
use crate::state::AppState;

/// Renames or moves a channel category.
#[utoipa::path(patch, path = "/api/v1/spaces/{space_id}/categories/{category_id}", tag = "spaces", summary = "Renames or moves a channel category.", request_body = UpdateCategoryRequest, params(("space_id" = SpaceId, Path), ("category_id" = CategoryId, Path)), responses((status = 200, body = CategoryResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn update_category_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath((space_id, category_id)): ApiPath<(SpaceId, CategoryId)>,
  ApiJson(request): ApiJson<UpdateCategoryRequest>,
) -> Result<Json<CategoryResponse>, ApiError> {
  let response = spaces::update_category(&state, &caller, space_id, category_id, request).await?;
  Ok(Json(response))
}
