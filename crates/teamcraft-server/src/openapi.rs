//! OpenAPI document of the REST API, generated from the handler annotations and proto types.

use axum::Json;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use teamcraft_proto::{
  AddConversationMemberRequest, AddSpaceMemberRequest, Base64Bytes, CategoryId, CategoryResponse,
  ChannelModeration, ChannelRole, ClientMessageId, ConversationId, ConversationKind,
  ConversationMemberResponse, ConversationResponse, ConversationVisibility, CreateCategoryRequest,
  CreateChannelRequest, CreateDmRequest, CreateGroupDmRequest, CreateInviteRequest, CreateSpaceRequest,
  DeviceActivity, DeviceId, DeviceResponse, ErrorBody, ErrorCode, GetMeResponse, HealthResponse,
  HealthStatus, HistoryPolicy, InviteId, InviteResponse, JoinPolicy, ListCategoriesResponse,
  ListConversationMembersResponse, ListConversationsQuery, ListConversationsResponse, ListDevicesResponse,
  ListInvitesResponse, ListMessagesQuery, ListMessagesResponse, ListPresenceQuery, ListPresenceResponse,
  ListSpaceMembersResponse, ListSpacesResponse, ManualPresence, MessageResponse, MyMembership,
  NotificationLevel, Permission, PresenceState, RegisterDeviceRequest, Role, SearchUsersQuery,
  SearchUsersResponse, SendMessageRequest, SetPresenceRequest, SetPresenceResponse, SetReadPositionRequest,
  SpaceId, SpaceMemberResponse, SpaceResponse, SpaceRole, UnixMillis, UpdateCategoryRequest,
  UpdateConversationMemberRequest, UpdateConversationRequest, UpdateSpaceMemberRequest, UpdateSpaceRequest,
  UserId, UserKind, UserPresence, UserSummary,
};

#[derive(OpenApi)]
#[openapi(
  info(title = "TeamCraft API", description = "REST API of the TeamCraft server. Realtime events: WebSocket /api/v1/gateway, frames in teamcraft-proto."),
  paths(crate::handlers::get_me_handler, crate::handlers::register_device_handler, crate::handlers::list_devices_handler, crate::handlers::revoke_device_handler, crate::handlers::search_users_handler, crate::handlers::get_user_handler, crate::handlers::list_spaces_handler, crate::handlers::create_space_handler, crate::handlers::get_space_handler, crate::handlers::update_space_handler, crate::handlers::join_space_handler, crate::handlers::list_space_members_handler, crate::handlers::add_space_member_handler, crate::handlers::update_space_member_handler, crate::handlers::remove_space_member_handler, crate::handlers::list_categories_handler, crate::handlers::create_category_handler, crate::handlers::update_category_handler, crate::handlers::delete_category_handler, crate::handlers::create_invite_handler, crate::handlers::list_invites_handler, crate::handlers::delete_invite_handler, crate::handlers::list_conversations_handler, crate::handlers::get_conversation_handler, crate::handlers::create_channel_handler, crate::handlers::create_dm_handler, crate::handlers::create_group_dm_handler, crate::handlers::update_conversation_handler, crate::handlers::list_conversation_members_handler, crate::handlers::add_conversation_member_handler, crate::handlers::update_conversation_member_handler, crate::handlers::remove_conversation_member_handler, crate::handlers::join_conversation_handler, crate::handlers::set_read_position_handler, crate::handlers::send_message_handler, crate::handlers::list_messages_handler, crate::handlers::set_presence_handler, crate::handlers::list_presence_handler),
  components(schemas(AddConversationMemberRequest, AddSpaceMemberRequest, Base64Bytes, CategoryId, CategoryResponse, ChannelModeration, ChannelRole, ClientMessageId, ConversationId, ConversationKind, ConversationMemberResponse, ConversationResponse, ConversationVisibility, CreateCategoryRequest, CreateChannelRequest, CreateDmRequest, CreateGroupDmRequest, CreateInviteRequest, CreateSpaceRequest, DeviceActivity, DeviceId, DeviceResponse, ErrorBody, ErrorCode, GetMeResponse, HealthResponse, HealthStatus, HistoryPolicy, InviteId, InviteResponse, JoinPolicy, ListCategoriesResponse, ListConversationMembersResponse, ListConversationsQuery, ListConversationsResponse, ListDevicesResponse, ListInvitesResponse, ListMessagesQuery, ListMessagesResponse, ListPresenceQuery, ListPresenceResponse, ListSpaceMembersResponse, ListSpacesResponse, ManualPresence, MessageResponse, MyMembership, NotificationLevel, Permission, PresenceState, RegisterDeviceRequest, Role, SearchUsersQuery, SearchUsersResponse, SendMessageRequest, SetPresenceRequest, SetPresenceResponse, SetReadPositionRequest, SpaceId, SpaceMemberResponse, SpaceResponse, SpaceRole, UnixMillis, UpdateCategoryRequest, UpdateConversationMemberRequest, UpdateConversationRequest, UpdateSpaceMemberRequest, UpdateSpaceRequest, UserId, UserKind, UserPresence, UserSummary)),
  modifiers(&BearerAuth),
)]
struct ApiDoc;

struct BearerAuth;

impl Modify for BearerAuth {
  fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
    let scheme = SecurityScheme::Http(
      HttpBuilder::new()
        .scheme(HttpAuthScheme::Bearer)
        .bearer_format("JWT")
        .build(),
    );
    openapi
      .components
      .get_or_insert_with(Default::default)
      .add_security_scheme("bearer", scheme);
  }
}

/// `GET /api/v1/openapi.json`
pub async fn openapi_json_handler() -> Json<utoipa::openapi::OpenApi> {
  Json(ApiDoc::openapi())
}

pub fn openapi() -> utoipa::openapi::OpenApi {
  ApiDoc::openapi()
}
