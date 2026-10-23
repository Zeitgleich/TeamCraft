use axum::Json;
use axum::extract::State;
use teamcraft_proto::{AdminBrandsResponse, BrandAssetSlot, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::brands;
use crate::state::AppState;

/// Removes an image of a database brand.
#[utoipa::path(delete, path = "/api/v1/admin/brands/{brand_id}/assets/{slot}", tag = "admin", summary = "Removes an image of a database brand.", params(("brand_id" = String, Path, description = "Brand id"), ("slot" = String, Path, description = "Image slot")), responses((status = 200, body = AdminBrandsResponse), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_delete_brand_asset_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath((brand_id, slot)): ApiPath<(String, String)>,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let response = brands::delete_asset(&state, &caller, &brand_id, &BrandAssetSlot::from(slot)).await?;
  Ok(Json(response))
}
