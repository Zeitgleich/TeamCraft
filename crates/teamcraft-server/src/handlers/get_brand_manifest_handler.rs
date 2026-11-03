use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use teamcraft_proto::ErrorBody;

use super::get_brand_handler::set_public_cache;
use crate::brand::ResolvedBrand;

/// Returns the web app manifest of the brand: name, theme color and icons for installing the web
/// client (docs/branding.md).
#[utoipa::path(get, path = "/api/v1/brand/manifest.webmanifest", tag = "brand", summary = "Returns the web app manifest.", responses((status = 200, content_type = "application/manifest+json", body = String), (status = 421, body = ErrorBody)))]
pub async fn get_brand_manifest_handler(resolved: ResolvedBrand) -> Response {
  let mut response = (
    [(
      header::CONTENT_TYPE,
      HeaderValue::from_static("application/manifest+json"),
    )],
    resolved.brand.web_manifest.clone(),
  )
    .into_response();
  set_public_cache(&mut response);
  response
}
