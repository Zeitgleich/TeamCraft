use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use teamcraft_proto::{BrandAssetSlot, ErrorBody};

use super::get_brand_handler::set_public_cache;
use crate::brand::ResolvedBrand;
use crate::error::ApiError;
use crate::extract::ApiPath;

/// Returns one image of the brand (`logo`, `logo_dark`, `icon`, `icon_dark`, `favicon`, `favicon_dark`).
#[utoipa::path(get, path = "/api/v1/brand/assets/{slot}", tag = "brand", summary = "Returns a brand image.", params(("slot" = String, Path, description = "Image slot")), responses((status = 200, content_type = "image/*", body = Vec<u8>), (status = 304), (status = 404, body = ErrorBody), (status = 421, body = ErrorBody)))]
pub async fn get_brand_asset_handler(
  resolved: ResolvedBrand,
  ApiPath(slot): ApiPath<String>,
  request_headers: HeaderMap,
) -> Result<Response, ApiError> {
  // The slot is an enum: a client-supplied name never reaches the file system.
  let slot = BrandAssetSlot::from(slot);
  let asset = resolved
    .brand
    .asset(&slot)
    .ok_or_else(ApiError::brand_asset_not_found)?;
  let is_fresh = request_headers
    .get(header::IF_NONE_MATCH)
    .is_some_and(|candidate| *candidate == asset.etag);
  let mut response = if is_fresh {
    StatusCode::NOT_MODIFIED.into_response()
  } else {
    Response::new(Body::from(asset.bytes.clone()))
  };
  let headers = response.headers_mut();
  headers.insert(header::ETAG, asset.etag.clone());
  if !is_fresh {
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(asset.content_type));
  }
  // The API guard adds `default-src 'none'`, so an SVG opened as a page cannot run script.
  set_public_cache(&mut response);
  Ok(response)
}
