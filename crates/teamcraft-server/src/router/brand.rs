//! The public brand of the host name a client talks to (docs/branding.md).

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(handlers::get_brand_handler))
    .routes(routes!(handlers::get_brand_asset_handler))
    .routes(routes!(handlers::get_brand_manifest_handler))
    .routes(routes!(handlers::get_brand_theme_handler))
    .routes(routes!(handlers::list_brands_handler))
}
