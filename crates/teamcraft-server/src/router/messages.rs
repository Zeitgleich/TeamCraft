//! Messages of a conversation.

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::ListMessagesQuery;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(
      handlers::list_messages_handler,
      handlers::send_message_handler
    ))
    .routes(routes!(handlers::schedule_message_handler))
    .routes(routes!(handlers::list_scheduled_messages_handler))
    .routes(routes!(
      handlers::update_scheduled_message_handler,
      handlers::cancel_scheduled_message_handler
    ))
    .routes(routes!(handlers::send_scheduled_message_handler))
}

/// Schemas in the document that no documented route names: query parameters.
#[derive(OpenApi)]
#[openapi(components(schemas(ListMessagesQuery)))]
struct Schemas;
