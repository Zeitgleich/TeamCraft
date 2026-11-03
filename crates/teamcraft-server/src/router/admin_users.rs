//! People in the admin console: deactivate, reactivate, delete, devices, push health.

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::ListAdminUsersQuery;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::admin_list_users_handler))
    .routes(routes!(handlers::admin_delete_user_handler))
    .routes(routes!(handlers::admin_deactivate_user_handler))
    .routes(routes!(handlers::admin_list_user_devices_handler))
    .routes(routes!(handlers::admin_revoke_device_handler))
    .routes(routes!(handlers::admin_get_push_health_handler))
    .routes(routes!(handlers::admin_reactivate_user_handler))
    .routes(routes!(handlers::admin_remove_user_from_chats_handler))
}

/// Schemas in the document that no documented route names: query parameters.
#[derive(OpenApi)]
#[openapi(components(schemas(ListAdminUsersQuery)))]
struct Schemas;
