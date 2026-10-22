use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use teamcraft_proto::{CategoryResponse, CreateCategoryRequest, ErrorBody, SpaceId};

use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::extract::{ApiJson, ApiPath};
use crate::services::spaces;
use crate::state::AppState;

/// Creates a channel category.
#[utoipa::path(post, path = "/api/v1/spaces/{space_id}/categories", tag = "spaces", summary = "Creates a channel category.", request_body = CreateCategoryRequest, params(("space_id" = SpaceId, Path)), responses((status = 201, body = CategoryResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn create_category_handler(
  State(state): State<AppState>,
  caller: AuthenticatedUser,
  ApiPath(space_id): ApiPath<SpaceId>,
  ApiJson(request): ApiJson<CreateCategoryRequest>,
) -> Result<(StatusCode, Json<CategoryResponse>), ApiError> {
  let response = spaces::create_category(&state, &caller, space_id, request).await?;
  Ok((StatusCode::CREATED, Json(response)))
}
