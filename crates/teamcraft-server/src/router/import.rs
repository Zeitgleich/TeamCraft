//! The import account's endpoints (admin.md §3.8, issue #402).

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(handlers::admin_import_provision_users_handler))
    .routes(routes!(handlers::admin_import_dm_handler))
    .routes(routes!(handlers::admin_import_messages_handler))
    .routes(routes!(handlers::admin_import_member_state_handler))
    .routes(routes!(handlers::admin_finish_import_handler))
    .routes(routes!(handlers::admin_remove_importer_handler))
}
