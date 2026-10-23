use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminBrandsResponse, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::brands;
use crate::state::AppState;

/// Deletes a database brand; a configuration brand with the same id comes back.
#[utoipa::path(delete, path = "/api/v1/admin/brands/{brand_id}", tag = "admin", summary = "Deletes a database brand and its images.", params(("brand_id" = String, Path, description = "Brand id")), responses((status = 200, body = AdminBrandsResponse), (status = 400, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_delete_brand_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath(brand_id): ApiPath<String>,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let response = brands::delete_brand(&state, &caller, &brand_id).await?;
  Ok(Json(response))
}
