//! The instance announcement as members see it.

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(handlers::get_announcement_handler))
    .routes(routes!(handlers::dismiss_announcement_handler))
}
