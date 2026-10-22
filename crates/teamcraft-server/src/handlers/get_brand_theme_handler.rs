use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use teamcraft_proto::ErrorBody;

use super::get_brand_handler::set_public_cache;
use crate::brand::ResolvedBrand;

/// Returns the brand's colors as a stylesheet of CSS custom properties.
#[utoipa::path(get, path = "/api/v1/brand/theme.css", tag = "brand", summary = "Returns the brand colors as CSS.", responses((status = 200, content_type = "text/css", body = String), (status = 421, body = ErrorBody)))]
pub async fn get_brand_theme_handler(resolved: ResolvedBrand) -> Response {
  let mut response = (
    [(
      header::CONTENT_TYPE,
      HeaderValue::from_static("text/css; charset=utf-8"),
    )],
    resolved.brand.theme_css.clone(),
  )
    .into_response();
  set_public_cache(&mut response);
  response
}
