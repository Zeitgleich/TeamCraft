use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use teamcraft_proto::{BrandAssetSlot, ErrorBody};

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiPath;
use crate::services::brands;
use crate::state::AppState;

/// Returns one image of any brand, for the preview in the admin console.
#[utoipa::path(get, path = "/api/v1/admin/brands/{brand_id}/assets/{slot}", tag = "admin", summary = "Returns an image of a brand for the admin console.", params(("brand_id" = String, Path, description = "Brand id"), ("slot" = String, Path, description = "Image slot")), responses((status = 200, content_type = "image/*", body = Vec<u8>), (status = 401, body = ErrorBody), (status = 403, body = ErrorBody), (status = 404, body = ErrorBody)), security(("bearer" = [])))]
pub async fn admin_get_brand_asset_handler(
  State(state): State<AppState>,
  AdminUser(caller): AdminUser,
  ApiPath((brand_id, slot)): ApiPath<(String, String)>,
) -> Result<Response, ApiError> {
  // The slot is an enum: a client-supplied name never reaches the storage.
  let asset = brands::get_asset(&state, &caller, &brand_id, &BrandAssetSlot::from(slot)).await?;
  let mut response = Response::new(Body::from(asset.bytes));
  let headers = response.headers_mut();
  headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
  headers.insert(
    header::CACHE_CONTROL,
    HeaderValue::from_static("private, no-store"),
  );
  Ok(response)
}
