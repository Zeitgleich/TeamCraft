use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminBrandsResponse, CreateBrandRequest, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::services::brands;
use crate::state::AppState;

/// Creates a brand in the database; its host names must be served host names.
#[utoipa::path(post, path = "/api/v1/admin/brands", tag = "admin", summary = "Creates a brand in the database.", request_body = CreateBrandRequest, responses((status = 200, body = AdminBrandsResponse), (status = 400, body = ErrorBody), (status = 409, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_create_brand_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiJson(request): ApiJson<CreateBrandRequest>,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let response = brands::create_brand(&state, &caller, request).await?;
  Ok(Json(response))
}
