//! Channels and direct conversations: settings, members, joining, admin join requests.

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use teamcraft_proto::{
  AdminJoinRequestChange, ChannelRoleAssignment, ChannelRolesDocument, ListChannelRoleLogQuery,
  ListConversationsQuery, ListSystemEventsQuery,
};

use crate::handlers;
use crate::state::AppState;

pub(super) fn routes() -> OpenApiRouter<AppState> {
  OpenApiRouter::with_openapi(Schemas::openapi())
    .routes(routes!(handlers::list_admin_join_requests_handler))
    .routes(routes!(handlers::list_conversations_handler))
    .routes(routes!(
      handlers::get_conversation_handler,
      handlers::update_conversation_handler
    ))
    .routes(routes!(handlers::request_admin_join_handler))
    .routes(routes!(handlers::close_admin_join_handler))
    .routes(routes!(handlers::approve_admin_join_handler))
    .routes(routes!(
      handlers::get_channel_image_handler,
      handlers::put_channel_image_handler,
      handlers::delete_channel_image_handler
    ))
    .routes(routes!(
      handlers::list_channel_role_log_handler,
      handlers::append_channel_role_log_handler
    ))
    .routes(routes!(handlers::join_conversation_handler))
    .routes(routes!(
      handlers::list_conversation_members_handler,
      handlers::add_conversation_member_handler
    ))
    .routes(routes!(
      handlers::update_conversation_member_handler,
      handlers::remove_conversation_member_handler
    ))
    .routes(routes!(handlers::check_member_removal_handler))
    .routes(routes!(handlers::set_read_position_handler))
    .routes(routes!(handlers::move_channel_handler))
    .routes(routes!(handlers::list_system_events_handler))
    .routes(routes!(handlers::convert_channel_privacy_handler))
    .routes(routes!(handlers::create_dm_handler))
    .routes(routes!(handlers::create_group_dm_handler))
    .routes(routes!(handlers::create_channel_handler))
}

/// Schemas in the document that no documented route names: query parameters, a gateway event's payload and the
/// signed channel roles document.
#[derive(OpenApi)]
#[openapi(components(schemas(
  AdminJoinRequestChange,
  ChannelRoleAssignment,
  ChannelRolesDocument,
  ListChannelRoleLogQuery,
  ListConversationsQuery,
  ListSystemEventsQuery
)))]
struct Schemas;
