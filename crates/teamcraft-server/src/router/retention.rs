//! Retention of spaces and conversations, and pinned messages, which it keeps.

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::pin_message_handler,
      handlers::unpin_message_handler
    ))
    .routes(routes!(
      handlers::get_conversation_retention_handler,
      handlers::set_conversation_retention_handler
    ))
    .routes(routes!(
      handlers::get_space_retention_handler,
      handlers::set_space_retention_handler
    ))
}
