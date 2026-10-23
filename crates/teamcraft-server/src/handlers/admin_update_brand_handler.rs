use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminBrandsResponse, ErrorBody, UpdateBrandRequest};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::extract::ApiPath;
use crate::services::brands;
use crate::state::AppState;

/// Replaces the settings of a database brand; configuration brands are read-only.
#[utoipa::path(put, path = "/api/v1/admin/brands/{brand_id}", tag = "admin", summary = "Replaces the settings of a database brand.", params(("brand_id" = String, Path, description = "Brand id")), request_body = UpdateBrandRequest, responses((status = 200, body = AdminBrandsResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_update_brand_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(brand_id): ApiPath<String>,
  ApiJson(request): ApiJson<UpdateBrandRequest>,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let response = brands::update_brand(&state, &caller, &brand_id, request).await?;
  Ok(Json(response))
}
