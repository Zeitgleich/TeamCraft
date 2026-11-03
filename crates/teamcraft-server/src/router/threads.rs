//! Threads and unread counts (docs/notifications.md).

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::ListThreadsQuery;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(
      handlers::follow_thread_handler,
      handlers::unfollow_thread_handler
    ))
    .routes(routes!(handlers::set_thread_read_handler))
    .routes(routes!(handlers::list_threads_handler))
    .routes(routes!(handlers::get_unread_handler))
}

/// Schemas in the document that no documented route names: query parameters.
#[derive(OpenApi)]
#[openapi(components(schemas(ListThreadsQuery)))]
struct Schemas;
