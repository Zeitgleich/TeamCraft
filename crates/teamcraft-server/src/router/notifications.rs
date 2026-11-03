//! Notification settings, read receipts and acknowledgements (docs/notifications.md).

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(handlers::acknowledge_message_handler))
    .routes(routes!(handlers::list_acknowledgements_handler))
    .routes(routes!(
      handlers::get_conversation_notification_settings_handler,
      handlers::put_conversation_notification_settings_handler
    ))
    .routes(routes!(handlers::get_read_receipts_handler))
    .routes(routes!(
      handlers::get_email_digest_handler,
      handlers::put_email_digest_handler
    ))
    .routes(routes!(
      handlers::get_notification_settings_handler,
      handlers::put_notification_settings_handler
    ))
    .routes(routes!(
      handlers::get_space_notification_settings_handler,
      handlers::put_space_notification_settings_handler
    ))
}
