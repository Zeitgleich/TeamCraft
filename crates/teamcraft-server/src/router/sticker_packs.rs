//! Sticker packs of the workspace (issue #2132).

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(handlers::list_sticker_packs_handler))
    .routes(routes!(
      handlers::save_sticker_pack_handler,
      handlers::delete_sticker_pack_handler
    ))
    .routes(routes!(handlers::upload_sticker_picture_handler))
    .routes(routes!(handlers::download_sticker_picture_handler))
}
