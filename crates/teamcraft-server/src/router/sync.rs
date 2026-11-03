//! The realtime gateway and the catch-up heads.

use axum::routing::get;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(handlers::get_sync_heads_handler))
    // A WebSocket; its frames are in teamcraft-proto (named in the document's description).
    .route("/api/v1/gateway", get(handlers::gateway_handler))
}
