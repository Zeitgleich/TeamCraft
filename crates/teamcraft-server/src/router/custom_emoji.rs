//! Custom emoji of the workspace (issue #207).

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::list_custom_emoji_handler,
      handlers::create_custom_emoji_handler
    ))
    .routes(routes!(
      handlers::rename_custom_emoji_handler,
      handlers::delete_custom_emoji_handler
    ))
    .routes(routes!(handlers::download_custom_emoji_handler))
}
