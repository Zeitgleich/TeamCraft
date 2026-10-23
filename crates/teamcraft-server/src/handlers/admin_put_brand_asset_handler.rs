use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use teamcraft_proto::{AdminBrandsResponse, BrandAssetSlot, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::brands;
use crate::state::AppState;

/// Sets an image of a database brand. The body is the image file; its `Content-Type` must match it.
#[utoipa::path(put, path = "/api/v1/admin/brands/{brand_id}/assets/{slot}", tag = "admin", summary = "Sets an image of a database brand.", params(("brand_id" = String, Path, description = "Brand id"), ("slot" = String, Path, description = "Image slot")), request_body(content = Vec<u8>, content_type = "image/*"), responses((status = 200, body = AdminBrandsResponse), (status = 400, body = ErrorBody), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody), (status = 409, body = ErrorBody), (status = 413, body = ErrorBody), (status = 503, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_put_brand_asset_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath((brand_id, slot)): ApiPath<(String, String)>,
  headers: HeaderMap,
  body: Bytes,
) -> Result<Json<AdminBrandsResponse>, ApiError> {
  let declared_type = headers
    .get(header::CONTENT_TYPE)
    .and_then(|value| value.to_str().ok())
    .unwrap_or_default();
  let response = brands::put_asset(
    &state,
    &caller,
    &brand_id,
    &BrandAssetSlot::from(slot),
    declared_type,
    body,
  )
  .await?;
  Ok(Json(response))
}
