//! Runtime settings in the admin console: general, mail and calendar.

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::admin_get_settings_handler,
      handlers::admin_put_settings_handler
    ))
    .routes(routes!(
      handlers::admin_get_calendar_settings_handler,
      handlers::admin_put_calendar_settings_handler,
      handlers::admin_reset_calendar_settings_handler
    ))
    .routes(routes!(handlers::admin_test_calendar_handler))
    .routes(routes!(
      handlers::admin_get_mail_settings_handler,
      handlers::admin_put_mail_settings_handler,
      handlers::admin_reset_mail_settings_handler
    ))
    .routes(routes!(handlers::admin_send_test_mail_handler))
    .routes(routes!(handlers::admin_reset_settings_handler))
}
