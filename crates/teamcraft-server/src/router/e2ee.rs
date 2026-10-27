//! Keys and MLS: identity keys, key packages, device lists, key bags, commits and welcomes
//! (docs/e2ee.md).

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::ListMlsMessagesQuery;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::list_mls_messages_handler))
    .routes(routes!(handlers::post_mls_commit_handler))
    .routes(routes!(
      handlers::get_mls_group_info_handler,
      handlers::put_mls_group_info_handler
    ))
    .routes(routes!(handlers::post_mls_message_handler))
    .routes(routes!(
      handlers::key_package_status_handler,
      handlers::upload_key_packages_handler
    ))
    .routes(routes!(handlers::list_welcomes_handler))
    .routes(routes!(handlers::put_my_device_list_handler))
    .routes(routes!(handlers::put_identity_key_handler))
    .routes(routes!(
      handlers::get_key_bag_handler,
      handlers::put_key_bag_handler
    ))
    .routes(routes!(handlers::get_user_device_list_handler))
    .routes(routes!(handlers::list_user_devices_handler))
    .routes(routes!(handlers::get_user_enrollment_handler))
    .routes(routes!(handlers::get_identity_key_handler))
    .routes(routes!(handlers::claim_key_packages_handler))
}

/// Schemas in the document that no documented route names: query parameters.
#[derive(OpenApi)]
#[openapi(components(schemas(ListMlsMessagesQuery)))]
struct Schemas;
