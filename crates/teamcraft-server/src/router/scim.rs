//! SCIM 2.0 (docs/provisioning.md §7): the endpoint identity providers push to, with its own bearer
//! tokens, and the SCIM clients in the admin console. The SCIM routes stay out of the OpenAPI
//! document: their bodies are SCIM's (RFC 7643/7644), not TeamCraft's API types.

use axum::routing::get;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::new()
    .routes(routes!(
      handlers::admin_list_scim_clients_handler,
      handlers::admin_create_scim_client_handler
    ))
    .routes(routes!(handlers::admin_rotate_scim_client_token_handler))
    .routes(routes!(handlers::admin_delete_scim_client_handler))
    .route(
      "/scim/v2/Users",
      get(handlers::scim_list_users_handler).post(handlers::scim_create_user_handler),
    )
    .route(
      "/scim/v2/Users/{id}",
      get(handlers::scim_get_user_handler)
        .put(handlers::scim_replace_user_handler)
        .patch(handlers::scim_patch_user_handler)
        .delete(handlers::scim_delete_user_handler),
    )
    .route(
      "/scim/v2/Groups",
      get(handlers::scim_list_groups_handler).post(handlers::scim_create_group_handler),
    )
    .route(
      "/scim/v2/Groups/{id}",
      get(handlers::scim_get_group_handler)
        .put(handlers::scim_replace_group_handler)
        .patch(handlers::scim_patch_group_handler)
        .delete(handlers::scim_delete_group_handler),
    )
    .route(
      "/scim/v2/ServiceProviderConfig",
      get(handlers::scim_service_provider_config_handler),
    )
    .route(
      "/scim/v2/ResourceTypes",
      get(handlers::scim_list_resource_types_handler),
    )
    .route(
      "/scim/v2/ResourceTypes/{id}",
      get(handlers::scim_get_resource_type_handler),
    )
    .route("/scim/v2/Schemas", get(handlers::scim_list_schemas_handler))
    .route("/scim/v2/Schemas/{id}", get(handlers::scim_get_schema_handler))
    .route(
      "/scim/v2",
      axum::routing::any(handlers::scim_unknown_path_handler),
    )
    .route(
      "/scim/v2/{*rest}",
      axum::routing::any(handlers::scim_unknown_path_handler),
    )
}
