use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use teamcraft_proto::ErrorBody;

use super::get_brand_handler::set_public_cache;
use crate::brand::ResolvedBrand;
use crate::services::app_links;
use crate::state::AppState;

/// Returns the apps that may open this host's links on iOS (docs/app-links.md). Apple's CDN fetches
/// it, as JSON without a file extension and without redirects.
#[utoipa::path(get, path = "/.well-known/apple-app-site-association", tag = "brand", summary = "Returns the iOS apps that may open this host's links.", responses((status = 200, content_type = "application/json", body = String), (status = 404, description = "App links are off or no Apple team is set"), (status = 421, body = ErrorBody)))]
pub async fn get_apple_app_site_association_handler(
  _resolved: ResolvedBrand,
  State(state): State<AppState>,
) -> Response {
  let Some(body) = app_links::apple_site_association(&state.settings.get().app_links) else {
    return StatusCode::NOT_FOUND.into_response();
  };
  let mut response = Json(body).into_response();
  set_public_cache(&mut response);
  response
}
