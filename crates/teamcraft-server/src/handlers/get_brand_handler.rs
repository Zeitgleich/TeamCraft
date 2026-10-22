use axum::Json;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use teamcraft_proto::{BrandResponse, ErrorBody};

use crate::brand::ResolvedBrand;

/// Returns how the server presents itself on the host the request arrived on.
#[utoipa::path(get, path = "/api/v1/brand", tag = "brand", summary = "Returns the brand of the request host.", responses((status = 200, body = BrandResponse), (status = 421, body = ErrorBody)))]
pub async fn get_brand_handler(resolved: ResolvedBrand) -> Response {
  let mut response = Json(resolved.brand.response.clone()).into_response();
  set_public_cache(&mut response);
  response
}

/// Brand data is public and the same for every caller of a host; it changes only with the server
/// configuration, so a short shared cache is fine as long as it keys on the host.
pub(super) fn set_public_cache(response: &mut Response) {
  let headers = response.headers_mut();
  headers.insert(
    header::CACHE_CONTROL,
    HeaderValue::from_static("public, max-age=300"),
  );
  headers.insert(header::VARY, HeaderValue::from_static("Host, X-Forwarded-Host"));
}
