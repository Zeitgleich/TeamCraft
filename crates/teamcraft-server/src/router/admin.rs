//! The admin console: announcement, audit log, bots, channel moderation and owners, organization
//! backgrounds, the instance overview, permission schemes, spaces and the caller's security log
//! (docs/admin.md).

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::{ListAdminConversationsQuery, ListAuditQuery};

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(
      handlers::admin_set_announcement_handler,
      handlers::admin_clear_announcement_handler
    ))
    .routes(routes!(handlers::admin_list_audit_handler))
    .routes(routes!(handlers::create_org_background_upload_handler))
    .routes(routes!(handlers::delete_org_background_handler))
    .routes(routes!(handlers::complete_org_background_upload_handler))
    .routes(routes!(
      handlers::admin_list_bots_handler,
      handlers::admin_create_bot_handler
    ))
    .routes(routes!(handlers::admin_revoke_bot_handler))
    .routes(routes!(handlers::admin_rotate_bot_token_handler))
    .routes(routes!(handlers::admin_list_conversations_handler))
    .routes(routes!(handlers::admin_list_conversation_members_handler))
    .routes(routes!(
      handlers::admin_get_channel_moderation_handler,
      handlers::admin_set_channel_moderation_handler
    ))
    .routes(routes!(handlers::admin_assign_channel_owner_handler))
    .routes(routes!(handlers::admin_instance_handler))
    .routes(routes!(handlers::admin_reissue_local_activation_handler))
    .routes(routes!(
      handlers::admin_get_permission_scheme_handler,
      handlers::admin_update_permission_scheme_handler
    ))
    .routes(routes!(handlers::admin_list_spaces_handler))
    .routes(routes!(handlers::admin_list_space_members_handler))
    .routes(routes!(
      handlers::admin_get_space_permission_scheme_handler,
      handlers::admin_update_space_permission_scheme_handler
    ))
    .routes(routes!(handlers::get_security_log_handler))
}

/// Schemas in the document that no documented route names: query parameters.
#[derive(OpenApi)]
#[openapi(components(schemas(ListAdminConversationsQuery, ListAuditQuery)))]
struct Schemas;
