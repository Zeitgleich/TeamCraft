use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminBrandsResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::brands;
use crate::state::AppState;

/// Copies a configuration brand with its images into the database, where it can be edited.
#[utoipa::path(post, path = "/api/v1/admin/brands/{brand_id}/take-over", tag = "admin", summary = "Copies a configuration brand into the database.", params(("brand_id" = String, Path, description = "Brand id")), responses((status = 200, body = AdminBrandsResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 503, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_take_over_brand_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(brand_id): ApiPath<String>,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let response = brands::take_over_brand(&state, &caller, &brand_id).await?;
  Ok(Json(response))
}
