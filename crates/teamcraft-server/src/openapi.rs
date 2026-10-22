//! OpenAPI document of the REST API, generated from the handler annotations and proto types.

use axum::Json;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use teamcraft_proto::{
  AddConversationMemberRequest, AddSpaceMemberRequest, AppendWorkspaceLogRequest, AttachmentDownloadResponse,
  AttachmentId, AttachmentResponse, Base64Bytes, CallId, CallJoinTokenResponse, CategoryId, CategoryResponse,
  ChannelModeration, ChannelRole, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ClientMessageId,
  ConversationId, ConversationKind, ConversationMemberResponse, ConversationResponse, ConversationVisibility,
  CreateAttachmentRequest, CreateAttachmentResponse, CreateCallJoinTokenRequest, CreateCategoryRequest,
  CreateChannelRequest, CreateDmRequest, CreateGroupDmRequest, CreateInviteRequest, CreateSpaceRequest,
  DeactivateUserRequest, DeviceActivity, DeviceId, DeviceResponse, ErrorBody, ErrorCode, GetMeResponse,
  HealthResponse, HealthStatus, HistoryPolicy, IdentityKeyResponse, InviteId, InviteResponse, JoinPolicy,
  KeyBagResponse, KeyPackageStatusResponse, ListAdminConversationsQuery, ListAdminConversationsResponse,
  ListAdminDevicesResponse, ListAdminSpacesResponse, ListAdminUsersQuery, ListAdminUsersResponse,
  ListAuditQuery, ListAuditResponse, ListCategoriesResponse, ListConversationMembersResponse,
  ListConversationsQuery, ListConversationsResponse, ListDevicesResponse, ListInvitesResponse,
  ListMessagesQuery, ListMessagesResponse, ListMlsMessagesQuery, ListMlsMessagesResponse, ListPresenceQuery,
  ListPresenceResponse, ListSpaceMembersResponse, ListSpacesResponse, ListWorkspaceLogQuery,
  ListWorkspaceLogResponse, ManualPresence, MessageId, MessageResponse, MlsMessageKind, MlsMessageResponse,
  MlsWelcome, MyMembership, NotificationLevel, Permission, PostMlsCommitRequest, PostMlsCommitResponse,
  PostMlsMessageRequest, PresenceState, PushProvider, PutKeyBagRequest, RegisterDeviceRequest,
  RegisterPushRequest, RetentionStatusResponse, Role, SearchUsersQuery, SearchUsersResponse,
  SendMessageRequest, SetPresenceRequest, SetPresenceResponse, SetReadPositionRequest, SetRetentionRequest,
  SpaceId, SpaceMemberResponse, SpaceResponse, SpaceRole, UnixMillis, UpdateCategoryRequest,
  UpdateConversationMemberRequest, UpdateConversationRequest, UpdateSpaceMemberRequest, UpdateSpaceRequest,
  UploadIdentityKeyRequest, UploadKeyPackagesRequest, UserId, UserKind, UserPresence, UserSummary,
  WorkspaceLogEntryResponse, WorkspaceLogKind, WorkspacePolicyResponse,
};

#[derive(OpenApi)]
#[openapi(
  info(title = "TeamCraft API", description = "REST API of the TeamCraft server. Realtime events: WebSocket /api/v1/gateway, frames in teamcraft-proto."),
  paths(crate::handlers::get_me_handler, crate::handlers::register_device_handler, crate::handlers::list_devices_handler, crate::handlers::revoke_device_handler, crate::handlers::search_users_handler, crate::handlers::get_user_handler, crate::handlers::list_spaces_handler, crate::handlers::create_space_handler, crate::handlers::get_space_handler, crate::handlers::update_space_handler, crate::handlers::join_space_handler, crate::handlers::list_space_members_handler, crate::handlers::add_space_member_handler, crate::handlers::update_space_member_handler, crate::handlers::remove_space_member_handler, crate::handlers::list_categories_handler, crate::handlers::create_category_handler, crate::handlers::update_category_handler, crate::handlers::delete_category_handler, crate::handlers::create_invite_handler, crate::handlers::list_invites_handler, crate::handlers::delete_invite_handler, crate::handlers::list_conversations_handler, crate::handlers::get_conversation_handler, crate::handlers::create_channel_handler, crate::handlers::create_dm_handler, crate::handlers::create_group_dm_handler, crate::handlers::update_conversation_handler, crate::handlers::list_conversation_members_handler, crate::handlers::add_conversation_member_handler, crate::handlers::update_conversation_member_handler, crate::handlers::remove_conversation_member_handler, crate::handlers::join_conversation_handler, crate::handlers::set_read_position_handler, crate::handlers::send_message_handler, crate::handlers::list_messages_handler, crate::handlers::set_presence_handler, crate::handlers::list_presence_handler, crate::handlers::put_identity_key_handler, crate::handlers::get_identity_key_handler, crate::handlers::list_user_devices_handler, crate::handlers::upload_key_packages_handler, crate::handlers::key_package_status_handler, crate::handlers::claim_key_packages_handler, crate::handlers::post_mls_commit_handler, crate::handlers::post_mls_message_handler, crate::handlers::list_mls_messages_handler, crate::handlers::list_welcomes_handler, crate::handlers::put_key_bag_handler, crate::handlers::get_key_bag_handler, crate::handlers::register_push_handler, crate::handlers::unregister_push_handler, crate::handlers::create_attachment_handler, crate::handlers::complete_attachment_handler, crate::handlers::download_attachment_handler, crate::handlers::append_roster_handler, crate::handlers::list_roster_handler, crate::handlers::append_policy_handler, crate::handlers::get_policy_handler, crate::handlers::list_policy_log_handler, crate::handlers::admin_list_spaces_handler, crate::handlers::admin_list_space_members_handler, crate::handlers::admin_list_conversations_handler, crate::handlers::admin_list_conversation_members_handler, crate::handlers::admin_list_users_handler, crate::handlers::admin_list_user_devices_handler, crate::handlers::admin_deactivate_user_handler, crate::handlers::admin_reactivate_user_handler, crate::handlers::admin_revoke_device_handler, crate::handlers::admin_list_audit_handler, crate::handlers::get_security_log_handler, crate::handlers::get_space_retention_handler, crate::handlers::set_space_retention_handler, crate::handlers::get_conversation_retention_handler, crate::handlers::set_conversation_retention_handler, crate::handlers::pin_message_handler, crate::handlers::unpin_message_handler, crate::handlers::create_call_join_token_handler),
  components(schemas(AddConversationMemberRequest, AddSpaceMemberRequest, AppendWorkspaceLogRequest, AttachmentDownloadResponse, AttachmentId, AttachmentResponse, Base64Bytes, CategoryId, CategoryResponse, ChannelModeration, ChannelRole, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ClientMessageId, ConversationId, ConversationKind, ConversationMemberResponse, ConversationResponse, ConversationVisibility, CreateAttachmentRequest, CreateAttachmentResponse, CreateCategoryRequest, CreateChannelRequest, CreateDmRequest, CreateGroupDmRequest, CreateInviteRequest, CreateSpaceRequest, DeactivateUserRequest, DeviceActivity, DeviceId, DeviceResponse, ErrorBody, ErrorCode, GetMeResponse, HealthResponse, HealthStatus, HistoryPolicy, IdentityKeyResponse, InviteId, InviteResponse, JoinPolicy, KeyBagResponse, KeyPackageStatusResponse, ListAdminConversationsQuery, ListAdminConversationsResponse, ListAdminDevicesResponse, ListAdminSpacesResponse, ListAdminUsersQuery, ListAdminUsersResponse, ListAuditQuery, ListAuditResponse, ListCategoriesResponse, ListConversationMembersResponse, ListConversationsQuery, ListConversationsResponse, ListDevicesResponse, ListInvitesResponse, ListMessagesQuery, ListMessagesResponse, ListMlsMessagesQuery, ListMlsMessagesResponse, ListPresenceQuery, ListPresenceResponse, ListSpaceMembersResponse, ListSpacesResponse, ListWorkspaceLogQuery, ListWorkspaceLogResponse, ManualPresence, MessageId, MessageResponse, MlsMessageKind, MlsMessageResponse, MlsWelcome, MyMembership, NotificationLevel, Permission, PostMlsCommitRequest, PostMlsCommitResponse, PostMlsMessageRequest, PresenceState, PushProvider, PutKeyBagRequest, RegisterDeviceRequest, RegisterPushRequest, RetentionStatusResponse, Role, SearchUsersQuery, SearchUsersResponse, SendMessageRequest, SetPresenceRequest, SetPresenceResponse, SetReadPositionRequest, SetRetentionRequest, SpaceId, SpaceMemberResponse, SpaceResponse, SpaceRole, UnixMillis, UpdateCategoryRequest, UpdateConversationMemberRequest, UpdateConversationRequest, UpdateSpaceMemberRequest, UpdateSpaceRequest, UploadIdentityKeyRequest, UploadKeyPackagesRequest, UserId, UserKind, UserPresence, UserSummary, WorkspaceLogEntryResponse, WorkspaceLogKind, WorkspacePolicyResponse, CallId, CallJoinTokenResponse, CreateCallJoinTokenRequest)),
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
