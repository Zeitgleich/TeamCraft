//! Share links of files.

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::list_share_links_handler,
      handlers::create_share_link_handler
    ))
    .routes(routes!(
      handlers::get_share_info_handler,
      handlers::revoke_share_link_handler
    ))
    .routes(routes!(handlers::download_share_handler))
}
