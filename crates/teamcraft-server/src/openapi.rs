//! OpenAPI document of the REST API, generated from the handler annotations and proto types.

use axum::Json;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

use teamcraft_proto::{
  AddConversationMemberRequest, AddSpaceMemberRequest, AppendWorkspaceLogRequest, AttachmentDownloadResponse,
  AttachmentId, AttachmentResponse, Base64Bytes, BotResponse, BotTokenResponse, CallJoinTokenResponse,
  CallRosterEntry, CallStateResponse, CategoryId, CategoryResponse, ChannelModeration, ChannelRole,
  ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ClientMessageId, ConversationId, ConversationKind,
  ConversationMemberResponse, ConversationNotificationSettings, ConversationResponse, ConversationUnread,
  ConversationVisibility, CreateAttachmentRequest, CreateAttachmentResponse, CreateBotRequest,
  CreateCallJoinTokenRequest, CreateCategoryRequest, CreateChannelRequest, CreateDmRequest,
  CreateGroupDmRequest, CreateGuestLinkRequest, CreateInstantMeetingRequest, CreateInviteRequest,
  CreateMeetingRequest, CreateSpaceRequest, CustomStatus, DeactivateUserRequest, DeviceActivity, DeviceId,
  DeviceResponse, DndSchedule, DndWindow, ErrorBody, ErrorCode, GetMeResponse, GuestLinkResponse,
  HealthResponse, HealthStatus, HistoryPolicy, IdentityKeyResponse, InviteId, InviteResponse,
  JoinMeetingRequest, JoinMeetingResponse, JoinPolicy, KeyBagResponse, KeyPackageStatusResponse,
  ListAcknowledgementsResponse, ListAdminConversationsQuery, ListAdminConversationsResponse,
  ListAdminDevicesResponse, ListAdminSpacesResponse, ListAdminUsersQuery, ListAdminUsersResponse,
  ListAuditQuery, ListAuditResponse, ListBotsResponse, ListCategoriesResponse,
  ListConversationMembersResponse, ListConversationsQuery, ListConversationsResponse, ListDevicesResponse,
  ListInvitesResponse, ListLobbyResponse, ListMeetingsQuery, ListMeetingsResponse, ListMessagesQuery,
  ListMessagesResponse, ListMlsMessagesQuery, ListMlsMessagesResponse, ListPresenceQuery,
  ListPresenceResponse, ListSpaceMembersResponse, ListSpacesResponse, ListThreadsQuery, ListThreadsResponse,
  ListWorkspaceLogQuery, ListWorkspaceLogResponse, LobbyEntry, LobbyState, LobbyStateResponse,
  ManualPresence, MeetingAttendeeResponse, MeetingId, MeetingInvitee, MeetingKind, MeetingRecurrence,
  MeetingReplyResponse, MeetingResponse, MeetingStatus, MessageAcknowledgement, MessageId, MessageResponse,
  MlsMessageKind, MlsMessageResponse, MlsWelcome, MyMembership, NotificationLevel, NotificationSettings,
  Permission, PostMlsCommitRequest, PostMlsCommitResponse, PostMlsMessageRequest, PresenceState, PushMode,
  PushProvider, PutKeyBagRequest, ReadReceipt, ReadReceiptsResponse, RecurrenceFrequency,
  RegisterDeviceRequest, RegisterPushRequest, RetentionStatusResponse, Role, RsvpStatus, SearchUsersQuery,
  SearchUsersResponse, SendMessageRequest, SetCustomStatusRequest, SetDeviceCertificateRequest,
  SetPresenceRequest, SetPresenceResponse, SetReadPositionRequest, SetRetentionRequest, SetRsvpRequest,
  SetThreadReadRequest, SfuParticipantReport, SpaceId, SpaceMemberResponse, SpaceResponse, SpaceRole,
  ThreadFollowReason, ThreadId, ThreadSummary, UnixMillis, UnreadResponse, UpdateCategoryRequest,
  UpdateConversationMemberRequest, UpdateConversationRequest, UpdateMeetingRequest, UpdateSpaceMemberRequest,
  UpdateSpaceRequest, UploadIdentityKeyRequest, UploadKeyPackagesRequest, UserId, UserKind, UserPresence,
  UserSummary, Weekday, WorkspaceLogEntryResponse, WorkspaceLogKind, WorkspacePolicyResponse,
};

#[derive(OpenApi)]
#[openapi(
  info(title = "TeamCraft API", description = "REST API of the TeamCraft server. Realtime events: WebSocket /api/v1/gateway, frames in teamcraft-proto."),
  paths(crate::handlers::get_me_handler, crate::handlers::register_device_handler, crate::handlers::list_devices_handler, crate::handlers::revoke_device_handler, crate::handlers::search_users_handler, crate::handlers::get_user_handler, crate::handlers::list_spaces_handler, crate::handlers::create_space_handler, crate::handlers::get_space_handler, crate::handlers::update_space_handler, crate::handlers::join_space_handler, crate::handlers::list_space_members_handler, crate::handlers::add_space_member_handler, crate::handlers::update_space_member_handler, crate::handlers::remove_space_member_handler, crate::handlers::list_categories_handler, crate::handlers::create_category_handler, crate::handlers::update_category_handler, crate::handlers::delete_category_handler, crate::handlers::create_invite_handler, crate::handlers::list_invites_handler, crate::handlers::delete_invite_handler, crate::handlers::list_conversations_handler, crate::handlers::get_conversation_handler, crate::handlers::create_channel_handler, crate::handlers::create_dm_handler, crate::handlers::create_group_dm_handler, crate::handlers::update_conversation_handler, crate::handlers::list_conversation_members_handler, crate::handlers::add_conversation_member_handler, crate::handlers::update_conversation_member_handler, crate::handlers::remove_conversation_member_handler, crate::handlers::join_conversation_handler, crate::handlers::set_read_position_handler, crate::handlers::send_message_handler, crate::handlers::list_messages_handler, crate::handlers::set_presence_handler, crate::handlers::list_presence_handler, crate::handlers::put_identity_key_handler, crate::handlers::get_identity_key_handler, crate::handlers::list_user_devices_handler, crate::handlers::upload_key_packages_handler, crate::handlers::key_package_status_handler, crate::handlers::claim_key_packages_handler, crate::handlers::post_mls_commit_handler, crate::handlers::post_mls_message_handler, crate::handlers::list_mls_messages_handler, crate::handlers::list_welcomes_handler, crate::handlers::put_key_bag_handler, crate::handlers::get_key_bag_handler, crate::handlers::register_push_handler, crate::handlers::unregister_push_handler, crate::handlers::create_attachment_handler, crate::handlers::complete_attachment_handler, crate::handlers::download_attachment_handler, crate::handlers::append_roster_handler, crate::handlers::list_roster_handler, crate::handlers::append_policy_handler, crate::handlers::get_policy_handler, crate::handlers::list_policy_log_handler, crate::handlers::admin_list_spaces_handler, crate::handlers::admin_list_space_members_handler, crate::handlers::admin_list_conversations_handler, crate::handlers::admin_list_conversation_members_handler, crate::handlers::admin_list_users_handler, crate::handlers::admin_list_user_devices_handler, crate::handlers::admin_deactivate_user_handler, crate::handlers::admin_reactivate_user_handler, crate::handlers::admin_revoke_device_handler, crate::handlers::admin_list_audit_handler, crate::handlers::get_security_log_handler, crate::handlers::get_space_retention_handler, crate::handlers::set_space_retention_handler, crate::handlers::get_conversation_retention_handler, crate::handlers::set_conversation_retention_handler, crate::handlers::pin_message_handler, crate::handlers::unpin_message_handler, crate::handlers::set_device_certificate_handler, crate::handlers::get_call_handler, crate::handlers::create_call_join_token_handler, crate::handlers::set_custom_status_handler, crate::handlers::clear_custom_status_handler, crate::handlers::list_threads_handler, crate::handlers::get_unread_handler, crate::handlers::follow_thread_handler, crate::handlers::unfollow_thread_handler, crate::handlers::set_thread_read_handler, crate::handlers::get_notification_settings_handler, crate::handlers::put_notification_settings_handler, crate::handlers::get_conversation_notification_settings_handler, crate::handlers::put_conversation_notification_settings_handler, crate::handlers::get_read_receipts_handler, crate::handlers::acknowledge_message_handler, crate::handlers::list_acknowledgements_handler, crate::handlers::create_meeting_handler, crate::handlers::create_instant_meeting_handler, crate::handlers::list_meetings_handler, crate::handlers::get_meeting_handler, crate::handlers::update_meeting_handler, crate::handlers::cancel_meeting_handler, crate::handlers::set_meeting_rsvp_handler, crate::handlers::create_guest_link_handler, crate::handlers::list_lobby_handler, crate::handlers::get_my_lobby_state_handler, crate::handlers::admit_guest_handler, crate::handlers::deny_guest_handler, crate::handlers::admin_create_bot_handler, crate::handlers::admin_list_bots_handler, crate::handlers::admin_rotate_bot_token_handler, crate::handlers::admin_revoke_bot_handler),
  components(schemas(AddConversationMemberRequest, AddSpaceMemberRequest, AppendWorkspaceLogRequest, AttachmentDownloadResponse, AttachmentId, AttachmentResponse, Base64Bytes, BotResponse, BotTokenResponse, CallJoinTokenResponse, CallRosterEntry, CallStateResponse, CategoryId, CategoryResponse, ChannelModeration, ChannelRole, ClaimKeyPackagesRequest, ClaimKeyPackagesResponse, ClientMessageId, ConversationId, ConversationKind, ConversationMemberResponse, ConversationNotificationSettings, ConversationResponse, ConversationUnread, ConversationVisibility, CreateAttachmentRequest, CreateAttachmentResponse, CreateBotRequest, CreateCallJoinTokenRequest, CreateCategoryRequest, CreateChannelRequest, CreateDmRequest, CreateGroupDmRequest, CreateGuestLinkRequest, CreateInstantMeetingRequest, CreateInviteRequest, CreateMeetingRequest, CreateSpaceRequest, CustomStatus, DeactivateUserRequest, DeviceActivity, DeviceId, DeviceResponse, DndSchedule, DndWindow, ErrorBody, ErrorCode, GetMeResponse, GuestLinkResponse, HealthResponse, HealthStatus, HistoryPolicy, IdentityKeyResponse, InviteId, InviteResponse, JoinMeetingRequest, JoinMeetingResponse, JoinPolicy, KeyBagResponse, KeyPackageStatusResponse, ListAcknowledgementsResponse, ListAdminConversationsQuery, ListAdminConversationsResponse, ListAdminDevicesResponse, ListAdminSpacesResponse, ListAdminUsersQuery, ListAdminUsersResponse, ListAuditQuery, ListAuditResponse, ListBotsResponse, ListCategoriesResponse, ListConversationMembersResponse, ListConversationsQuery, ListConversationsResponse, ListDevicesResponse, ListInvitesResponse, ListLobbyResponse, ListMeetingsQuery, ListMeetingsResponse, ListMessagesQuery, ListMessagesResponse, ListMlsMessagesQuery, ListMlsMessagesResponse, ListPresenceQuery, ListPresenceResponse, ListSpaceMembersResponse, ListSpacesResponse, ListThreadsQuery, ListThreadsResponse, ListWorkspaceLogQuery, ListWorkspaceLogResponse, LobbyEntry, LobbyState, LobbyStateResponse, ManualPresence, MeetingAttendeeResponse, MeetingId, MeetingInvitee, MeetingKind, MeetingRecurrence, MeetingReplyResponse, MeetingResponse, MeetingStatus, MessageAcknowledgement, MessageId, MessageResponse, MlsMessageKind, MlsMessageResponse, MlsWelcome, MyMembership, NotificationLevel, NotificationSettings, Permission, PostMlsCommitRequest, PostMlsCommitResponse, PostMlsMessageRequest, PresenceState, PushMode, PushProvider, PutKeyBagRequest, ReadReceipt, ReadReceiptsResponse, RecurrenceFrequency, RegisterDeviceRequest, RegisterPushRequest, RetentionStatusResponse, Role, RsvpStatus, SearchUsersQuery, SearchUsersResponse, SendMessageRequest, SetCustomStatusRequest, SetDeviceCertificateRequest, SetPresenceRequest, SetPresenceResponse, SetReadPositionRequest, SetRetentionRequest, SetRsvpRequest, SetThreadReadRequest, SfuParticipantReport, SpaceId, SpaceMemberResponse, SpaceResponse, SpaceRole, ThreadFollowReason, ThreadId, ThreadSummary, UnixMillis, UnreadResponse, UpdateCategoryRequest, UpdateConversationMemberRequest, UpdateConversationRequest, UpdateMeetingRequest, UpdateSpaceMemberRequest, UpdateSpaceRequest, UploadIdentityKeyRequest, UploadKeyPackagesRequest, UserId, UserKind, UserPresence, UserSummary, Weekday, WorkspaceLogEntryResponse, WorkspaceLogKind, WorkspacePolicyResponse)),
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
