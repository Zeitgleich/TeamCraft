//! Outgoing webhooks in the admin console (docs/webhooks.md).

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::admin_list_webhooks_handler,
      handlers::admin_create_webhook_handler
    ))
    .routes(routes!(
      handlers::admin_update_webhook_handler,
      handlers::admin_delete_webhook_handler
    ))
    .routes(routes!(handlers::admin_rotate_webhook_secret_handler))
    .routes(routes!(handlers::admin_test_webhook_handler))
    .routes(routes!(handlers::admin_list_webhook_deliveries_handler))
}
