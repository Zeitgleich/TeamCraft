//! Brands in the admin console: per-host names, colors and assets (docs/branding.md).

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::admin_list_brands_handler,
      handlers::admin_create_brand_handler
    ))
    .routes(routes!(
      handlers::admin_update_brand_handler,
      handlers::admin_delete_brand_handler
    ))
    .routes(routes!(
      handlers::admin_get_brand_asset_handler,
      handlers::admin_put_brand_asset_handler,
      handlers::admin_delete_brand_asset_handler
    ))
    .routes(routes!(handlers::admin_take_over_brand_handler))
}
