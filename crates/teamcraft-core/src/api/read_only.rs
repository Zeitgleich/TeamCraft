//! A [`ServerApi`] that only reads (issue #839, e2ee.md §7.2): the iOS notification extension
//! catches a conversation up through it to show a message, next to an app that owns the account's
//! state. Whatever the catch-up would change on the server (a commit, a rejoin request, key
//! packages, read positions, a message) is refused before it leaves the device, by the method
//! itself: every method of the trait is either a read passed on or a refusal, so a new method
//! doesn't compile until it is sorted into one of them. Downloads of files and pictures are refused
//! too: a notification needs none, and the extension has little memory.

use std::sync::Mutex;

use super::*;

/// How many refused calls are remembered by name (for tests and a log line); later ones are only
/// counted.
const MAX_NAMED_REFUSALS: usize = 32;

/// `inner` with every call that changes something on the server refused.
pub struct ReadOnlyApi<A> {
  inner: A,
  refusals: Mutex<Refusals>,
}

#[derive(Default)]
struct Refusals {
  count: u64,
  names: Vec<&'static str>,
}

impl<A> ReadOnlyApi<A> {
  pub fn new(inner: A) -> Self {
    Self {
      inner,
      refusals: Mutex::new(Refusals::default()),
    }
  }

  /// How many calls were refused so far.
  pub fn refusal_count(&self) -> u64 {
    self.lock().count
  }

  /// The methods refused, in order, the first [`MAX_NAMED_REFUSALS`] of them.
  pub fn refused_calls(&self) -> Vec<&'static str> {
    self.lock().names.clone()
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Refusals> {
    // Plain counters: a panic elsewhere while it was locked leaves nothing half-changed.
    self
      .refusals
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }

  /// The answer of a refused call: never sent, and an error that is neither a network failure to
  /// retry nor a server's refusal (403, 404) that the client would act on.
  fn refused<T>(&self, name: &'static str) -> std::future::Ready<Result<T, ApiError>> {
    let mut refusals = self.lock();
    refusals.count += 1;
    if refusals.names.len() < MAX_NAMED_REFUSALS {
      refusals.names.push(name);
    }
    std::future::ready(Err(ApiError::Unsupported(name)))
  }
}

impl<A: ServerApi> ServerApi for ReadOnlyApi<A> {
  type Gateway = A::Gateway;
  type Rendezvous = A::Rendezvous;

  fn me(&self) -> impl Future<Output = Result<GetMeResponse, ApiError>> {
    self.inner.me()
  }

  fn user(&self, user: UserId) -> impl Future<Output = Result<UserSummary, ApiError>> {
    self.inner.user(user)
  }

  fn search_users(&self, query: &str) -> impl Future<Output = Result<SearchUsersResponse, ApiError>> {
    self.inner.search_users(query)
  }

  fn directory_people(
    &self,
    query: &DirectoryPeopleQuery,
  ) -> impl Future<Output = Result<DirectoryPeopleResponse, ApiError>> {
    self.inner.directory_people(query)
  }

  fn directory_filters(
    &self,
    query: &DirectoryFiltersQuery,
  ) -> impl Future<Output = Result<DirectoryFiltersResponse, ApiError>> {
    self.inner.directory_filters(query)
  }

  fn directory_channels(
    &self,
    query: &DirectoryChannelsQuery,
  ) -> impl Future<Output = Result<DirectoryChannelsResponse, ApiError>> {
    self.inner.directory_channels(query)
  }

  fn directory_invitations(
    &self,
    query: &DirectoryInvitationsQuery,
  ) -> impl Future<Output = Result<DirectoryInvitationsResponse, ApiError>> {
    self.inner.directory_invitations(query)
  }

  fn register_device(
    &self,
    _request: &RegisterDeviceRequest,
  ) -> impl Future<Output = Result<DeviceResponse, ApiError>> {
    self.refused("register_device")
  }

  fn list_user_devices(&self, user: UserId) -> impl Future<Output = Result<ListDevicesResponse, ApiError>> {
    self.inner.list_user_devices(user)
  }

  fn update_device(
    &self,
    _device: DeviceId,
    _request: &UpdateDeviceRequest,
  ) -> impl Future<Output = Result<DeviceResponse, ApiError>> {
    self.refused("update_device")
  }

  fn put_identity_key(
    &self,
    _request: &UploadIdentityKeyRequest,
  ) -> impl Future<Output = Result<IdentityKeyResponse, ApiError>> {
    self.refused("put_identity_key")
  }

  fn identity_key(
    &self,
    user: UserId,
  ) -> impl Future<Output = Result<Option<IdentityKeyResponse>, ApiError>> {
    self.inner.identity_key(user)
  }

  fn device_list(&self, user: UserId) -> impl Future<Output = Result<Option<DeviceListResponse>, ApiError>> {
    self.inner.device_list(user)
  }

  fn put_device_list(
    &self,
    _request: &PutDeviceListRequest,
  ) -> impl Future<Output = Result<DeviceListResponse, ApiError>> {
    self.refused("put_device_list")
  }

  fn revoke_device(&self, _device: DeviceId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("revoke_device")
  }

  fn announce_device_link(
    &self,
    _request: &CreateDeviceLinkRequest,
  ) -> impl Future<Output = Result<DeviceLinkRequest, ApiError>> {
    self.refused("announce_device_link")
  }

  fn device_links(&self) -> impl Future<Output = Result<Vec<DeviceLinkRequest>, ApiError>> {
    self.inner.device_links()
  }

  fn reject_device_link(&self, _request_id: &str) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("reject_device_link")
  }

  fn withdraw_device_link(&self, _request_id: &str) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("withdraw_device_link")
  }

  fn expiring_guests(&self) -> impl Future<Output = Result<Vec<ExpiringGuestResponse>, ApiError>> {
    self.inner.expiring_guests()
  }

  fn extend_guest(
    &self,
    _space: SpaceId,
    _user: UserId,
    _request: &ExtendGuestRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("extend_guest")
  }

  fn admin_join_requests(&self) -> impl Future<Output = Result<Vec<AdminJoinRequestResponse>, ApiError>> {
    self.inner.admin_join_requests()
  }

  fn request_admin_join(
    &self,
    _conversation: ConversationId,
  ) -> impl Future<Output = Result<AdminJoinRequestResponse, ApiError>> {
    self.refused("request_admin_join")
  }

  fn approve_admin_join(
    &self,
    _conversation: ConversationId,
    _user: UserId,
  ) -> impl Future<Output = Result<ConversationMemberResponse, ApiError>> {
    self.refused("approve_admin_join")
  }

  fn close_admin_join(
    &self,
    _conversation: ConversationId,
    _user: UserId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("close_admin_join")
  }

  fn upload_key_packages(
    &self,
    _device: DeviceId,
    _request: &UploadKeyPackagesRequest,
  ) -> impl Future<Output = Result<KeyPackageStatusResponse, ApiError>> {
    self.refused("upload_key_packages")
  }

  fn key_package_status(
    &self,
    device: DeviceId,
  ) -> impl Future<Output = Result<KeyPackageStatusResponse, ApiError>> {
    self.inner.key_package_status(device)
  }

  fn claim_key_packages(
    &self,
    _user: UserId,
    _request: &ClaimKeyPackagesRequest,
  ) -> impl Future<Output = Result<ClaimKeyPackagesResponse, ApiError>> {
    self.refused("claim_key_packages")
  }

  fn list_spaces(&self) -> impl Future<Output = Result<ListSpacesResponse, ApiError>> {
    self.inner.list_spaces()
  }

  fn list_space_members(
    &self,
    space: SpaceId,
  ) -> impl Future<Output = Result<ListSpaceMembersResponse, ApiError>> {
    self.inner.list_space_members(space)
  }

  fn list_conversations(&self) -> impl Future<Output = Result<ListConversationsResponse, ApiError>> {
    self.inner.list_conversations()
  }

  fn list_user_group_rosters(&self) -> impl Future<Output = Result<ListUserGroupRostersResponse, ApiError>> {
    self.inner.list_user_group_rosters()
  }

  fn list_custom_emoji(
    &self,
  ) -> impl Future<Output = Result<teamcraft_proto::ListCustomEmojiResponse, ApiError>> {
    self.inner.list_custom_emoji()
  }

  fn create_custom_emoji(
    &self,
    _request: &teamcraft_proto::CreateCustomEmojiRequest,
  ) -> impl Future<Output = Result<teamcraft_proto::CustomEmojiResponse, ApiError>> {
    self.refused("create_custom_emoji")
  }

  fn rename_custom_emoji(
    &self,
    _emoji: teamcraft_proto::CustomEmojiId,
    _request: &teamcraft_proto::RenameCustomEmojiRequest,
  ) -> impl Future<Output = Result<teamcraft_proto::CustomEmojiResponse, ApiError>> {
    self.refused("rename_custom_emoji")
  }

  fn delete_custom_emoji(
    &self,
    _emoji: teamcraft_proto::CustomEmojiId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("delete_custom_emoji")
  }

  fn download_custom_emoji(
    &self,
    _emoji: teamcraft_proto::CustomEmojiId,
  ) -> impl Future<Output = Result<teamcraft_proto::CustomEmojiDownloadResponse, ApiError>> {
    self.refused("download_custom_emoji")
  }

  fn conversation(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>> {
    self.inner.conversation(conversation)
  }

  fn list_members(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<ListConversationMembersResponse, ApiError>> {
    self.inner.list_members(conversation)
  }

  fn create_channel(
    &self,
    _space: SpaceId,
    _request: &CreateChannelRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>> {
    self.refused("create_channel")
  }

  fn create_dm(
    &self,
    _request: &CreateDmRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>> {
    self.refused("create_dm")
  }

  fn create_group_dm(
    &self,
    _request: &CreateGroupDmRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>> {
    self.refused("create_group_dm")
  }

  fn add_member(
    &self,
    _conversation: ConversationId,
    _request: &AddConversationMemberRequest,
  ) -> impl Future<Output = Result<ConversationMemberResponse, ApiError>> {
    self.refused("add_member")
  }

  fn check_member_removal(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.inner.check_member_removal(conversation, user)
  }

  fn remove_member(
    &self,
    _conversation: ConversationId,
    _user: UserId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("remove_member")
  }

  fn check_leave(
    &self,
    conversation: ConversationId,
    me: UserId,
    deletes_if_last: bool,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.inner.check_leave(conversation, me, deletes_if_last)
  }

  fn leave_conversation(
    &self,
    _conversation: ConversationId,
    _me: UserId,
    _deletes_if_last: bool,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("leave_conversation")
  }

  fn delete_conversation(
    &self,
    _conversation: ConversationId,
    _request: &DeleteConversationRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("delete_conversation")
  }

  fn admin_channels_of(&self, user: UserId) -> impl Future<Output = Result<Vec<ConversationId>, ApiError>> {
    self.inner.admin_channels_of(user)
  }

  fn admin_remove_leaver_from_chats(&self, _user: UserId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("admin_remove_leaver_from_chats")
  }

  fn admin_list_leavers(
    &self,
    query: &teamcraft_proto::ListLeaversQuery,
  ) -> impl Future<Output = Result<teamcraft_proto::ListLeaversResponse, ApiError>> {
    self.inner.admin_list_leavers(query)
  }

  fn admin_claim_leaver(
    &self,
    _user: UserId,
    _request: &teamcraft_proto::ClaimLeaverRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("admin_claim_leaver")
  }

  fn admin_remove_from_chats(&self, _user: UserId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("admin_remove_from_chats")
  }

  fn admin_delete_user(&self, _user: UserId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("admin_delete_user")
  }

  fn join_space(&self, _space: SpaceId) -> impl Future<Output = Result<SpaceResponse, ApiError>> {
    self.refused("join_space")
  }

  fn add_space_member(
    &self,
    _space: SpaceId,
    _request: &AddSpaceMemberRequest,
  ) -> impl Future<Output = Result<SpaceMemberResponse, ApiError>> {
    self.refused("add_space_member")
  }

  fn join_conversation(&self, _conversation: ConversationId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("join_conversation")
  }

  fn update_conversation(
    &self,
    _conversation: ConversationId,
    _request: &UpdateConversationRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>> {
    self.refused("update_conversation")
  }

  fn move_conversation(
    &self,
    _conversation: ConversationId,
    _request: &MoveChannelRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>> {
    self.refused("move_conversation")
  }

  fn list_joinable(
    &self,
    space: SpaceId,
  ) -> impl Future<Output = Result<ListConversationsResponse, ApiError>> {
    self.inner.list_joinable(space)
  }

  fn delete_space(&self, _space: SpaceId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("delete_space")
  }

  fn create_space(
    &self,
    _request: &CreateSpaceRequest,
  ) -> impl Future<Output = Result<SpaceResponse, ApiError>> {
    self.refused("create_space")
  }

  fn update_space(
    &self,
    _space: SpaceId,
    _request: &UpdateSpaceRequest,
  ) -> impl Future<Output = Result<SpaceResponse, ApiError>> {
    self.refused("update_space")
  }

  fn list_categories(
    &self,
    space: SpaceId,
  ) -> impl Future<Output = Result<ListCategoriesResponse, ApiError>> {
    self.inner.list_categories(space)
  }

  fn create_category(
    &self,
    _space: SpaceId,
    _request: &CreateCategoryRequest,
  ) -> impl Future<Output = Result<CategoryResponse, ApiError>> {
    self.refused("create_category")
  }

  fn update_category(
    &self,
    _space: SpaceId,
    _category: CategoryId,
    _request: &UpdateCategoryRequest,
  ) -> impl Future<Output = Result<CategoryResponse, ApiError>> {
    self.refused("update_category")
  }

  fn delete_category(
    &self,
    _space: SpaceId,
    _category: CategoryId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("delete_category")
  }

  fn create_invite(
    &self,
    _space: SpaceId,
    _request: &CreateInviteRequest,
  ) -> impl Future<Output = Result<InviteResponse, ApiError>> {
    self.refused("create_invite")
  }

  fn list_invites(&self, space: SpaceId) -> impl Future<Output = Result<ListInvitesResponse, ApiError>> {
    self.inner.list_invites(space)
  }

  fn delete_invite(&self, _space: SpaceId, _invite: InviteId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("delete_invite")
  }

  fn resend_invite(&self, _space: SpaceId, _invite: InviteId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("resend_invite")
  }

  fn update_space_member(
    &self,
    _space: SpaceId,
    _user: UserId,
    _request: &UpdateSpaceMemberRequest,
  ) -> impl Future<Output = Result<SpaceMemberResponse, ApiError>> {
    self.refused("update_space_member")
  }

  fn remove_space_member(
    &self,
    _space: SpaceId,
    _user: UserId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("remove_space_member")
  }

  fn update_member(
    &self,
    _conversation: ConversationId,
    _user: UserId,
    _request: &UpdateConversationMemberRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("update_member")
  }

  fn set_read_position(
    &self,
    _conversation: ConversationId,
    _request: &SetReadPositionRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("set_read_position")
  }

  fn mark_reactions_seen(
    &self,
    _conversation: ConversationId,
    _request: &teamcraft_proto::MarkReactionsSeenRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("mark_reactions_seen")
  }

  fn set_thread_read(
    &self,
    _conversation: ConversationId,
    _thread: teamcraft_proto::ThreadId,
    _request: &teamcraft_proto::SetThreadReadRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("set_thread_read")
  }

  fn set_presence(&self, _request: &SetPresenceRequest) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("set_presence")
  }

  fn set_custom_status(
    &self,
    _request: &teamcraft_proto::SetCustomStatusRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("set_custom_status")
  }

  fn clear_custom_status(&self) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("clear_custom_status")
  }

  fn list_presence(&self, users: &[UserId]) -> impl Future<Output = Result<ListPresenceResponse, ApiError>> {
    self.inner.list_presence(users)
  }

  fn get_privacy(&self) -> impl Future<Output = Result<PrivacyResponse, ApiError>> {
    self.inner.get_privacy()
  }

  fn put_privacy(
    &self,
    _request: &UpdatePrivacyRequest,
  ) -> impl Future<Output = Result<PrivacyResponse, ApiError>> {
    self.refused("put_privacy")
  }

  fn list_call_history(
    &self,
    query: &ListCallHistoryQuery,
  ) -> impl Future<Output = Result<ListCallHistoryResponse, ApiError>> {
    self.inner.list_call_history(query)
  }

  fn create_call_join_token(
    &self,
    _conversation: ConversationId,
    _request: &CreateCallJoinTokenRequest,
  ) -> impl Future<Output = Result<CallJoinTokenResponse, ApiError>> {
    self.refused("create_call_join_token")
  }

  fn running_call(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<Option<CallStateResponse>, ApiError>> {
    self.inner.running_call(conversation)
  }

  fn running_calls(&self) -> impl Future<Output = Result<Option<Vec<CallStateResponse>>, ApiError>> {
    self.inner.running_calls()
  }

  fn create_instant_meeting(
    &self,
    _request: &CreateInstantMeetingRequest,
  ) -> impl Future<Output = Result<MeetingResponse, ApiError>> {
    self.refused("create_instant_meeting")
  }

  fn cancel_meeting(&self, _meeting: MeetingId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("cancel_meeting")
  }

  fn put_meeting_admitters(
    &self,
    _conversation: ConversationId,
    _request: &PutMeetingAdmittersRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("put_meeting_admitters")
  }

  fn meeting_admitters(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<MeetingAdmittersResponse, ApiError>> {
    self.inner.meeting_admitters(conversation)
  }

  fn move_call(
    &self,
    _conversation: ConversationId,
    _request: &MoveCallRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("move_call")
  }

  fn send_message(
    &self,
    _conversation: ConversationId,
    _request: &SendMessageRequest,
  ) -> impl Future<Output = Result<MessageResponse, ApiError>> {
    self.refused("send_message")
  }

  fn provision_users(
    &self,
    _request: &ProvisionUsersRequest,
  ) -> impl Future<Output = Result<ProvisionUsersResponse, ApiError>> {
    self.refused("provision_users")
  }

  fn import_dm(
    &self,
    _request: &ImportDmRequest,
  ) -> impl Future<Output = Result<ConversationResponse, ApiError>> {
    self.refused("import_dm")
  }

  fn import_messages(
    &self,
    _conversation: ConversationId,
    _request: &ImportMessagesRequest,
  ) -> impl Future<Output = Result<ImportMessagesResponse, ApiError>> {
    self.refused("import_messages")
  }

  fn set_import_member_state(
    &self,
    _conversation: ConversationId,
    _user: UserId,
    _request: &ImportMemberStateRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("set_import_member_state")
  }

  fn finish_import(&self, _conversation: ConversationId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("finish_import")
  }

  fn admin_space_names(&self) -> impl Future<Output = Result<Vec<(SpaceId, String)>, ApiError>> {
    self.inner.admin_space_names()
  }

  fn admin_channel_names(
    &self,
    space: SpaceId,
  ) -> impl Future<Output = Result<Vec<(ConversationId, Option<String>)>, ApiError>> {
    self.inner.admin_channel_names(space)
  }

  fn admin_find_user(&self, username: &str) -> impl Future<Output = Result<Option<UserId>, ApiError>> {
    self.inner.admin_find_user(username)
  }

  fn list_messages(
    &self,
    conversation: ConversationId,
    query: &ListMessagesQuery,
  ) -> impl Future<Output = Result<ListMessagesResponse, ApiError>> {
    self.inner.list_messages(conversation, query)
  }

  fn set_message_pinned(
    &self,
    _conversation: ConversationId,
    _message: MessageId,
    _is_pinned: bool,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("set_message_pinned")
  }

  fn acknowledge_message(
    &self,
    _conversation: ConversationId,
    _message: MessageId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("acknowledge_message")
  }

  fn list_threads(
    &self,
    query: &ListThreadsQuery,
  ) -> impl Future<Output = Result<ListThreadsResponse, ApiError>> {
    self.inner.list_threads(query)
  }

  fn unread(&self) -> impl Future<Output = Result<UnreadResponse, ApiError>> {
    self.inner.unread()
  }

  fn sync_heads(&self) -> impl Future<Output = Result<SyncHeadsResponse, ApiError>> {
    self.inner.sync_heads()
  }

  fn read_receipts(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<teamcraft_proto::ReadReceiptsResponse, ApiError>> {
    self.inner.read_receipts(conversation)
  }

  fn post_mls_commit(
    &self,
    _conversation: ConversationId,
    _request: &PostMlsCommitRequest,
  ) -> impl Future<Output = Result<PostMlsCommitResponse, ApiError>> {
    self.refused("post_mls_commit")
  }

  fn post_mls_message(
    &self,
    _conversation: ConversationId,
    _request: &PostMlsMessageRequest,
  ) -> impl Future<Output = Result<MlsMessageResponse, ApiError>> {
    self.refused("post_mls_message")
  }

  fn list_mls_messages(
    &self,
    conversation: ConversationId,
    query: &ListMlsMessagesQuery,
  ) -> impl Future<Output = Result<ListMlsMessagesResponse, ApiError>> {
    self.inner.list_mls_messages(conversation, query)
  }

  fn list_welcomes(
    &self,
    device: DeviceId,
    query: &ListMlsMessagesQuery,
  ) -> impl Future<Output = Result<ListMlsMessagesResponse, ApiError>> {
    self.inner.list_welcomes(device, query)
  }

  fn mls_group_info(
    &self,
    conversation: ConversationId,
  ) -> impl Future<Output = Result<teamcraft_proto::MlsGroupInfoResponse, ApiError>> {
    self.inner.mls_group_info(conversation)
  }

  fn put_mls_group_info(
    &self,
    _conversation: ConversationId,
    _request: &teamcraft_proto::PutMlsGroupInfoRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("put_mls_group_info")
  }

  fn put_key_bag(
    &self,
    _request: &PutKeyBagRequest,
  ) -> impl Future<Output = Result<KeyBagResponse, ApiError>> {
    self.refused("put_key_bag")
  }

  fn key_bag(&self) -> impl Future<Output = Result<KeyBagResponse, ApiError>> {
    self.inner.key_bag()
  }

  fn schedule_message(
    &self,
    _conversation: ConversationId,
    _request: &ScheduleMessageRequest,
  ) -> impl Future<Output = Result<ScheduledMessageResponse, ApiError>> {
    self.refused("schedule_message")
  }

  fn list_scheduled_messages(&self) -> impl Future<Output = Result<ListScheduledMessagesResponse, ApiError>> {
    self.inner.list_scheduled_messages()
  }

  fn update_scheduled_message(
    &self,
    _id: ScheduledMessageId,
    _request: &UpdateScheduledMessageRequest,
  ) -> impl Future<Output = Result<ScheduledMessageResponse, ApiError>> {
    self.refused("update_scheduled_message")
  }

  fn cancel_scheduled_message(&self, _id: ScheduledMessageId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("cancel_scheduled_message")
  }

  fn send_scheduled_message(
    &self,
    _id: ScheduledMessageId,
  ) -> impl Future<Output = Result<SendScheduledMessageResponse, ApiError>> {
    self.refused("send_scheduled_message")
  }

  fn put_role_evidence(
    &self,
    _request: &teamcraft_proto::PutRoleEvidenceRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("put_role_evidence")
  }

  fn put_identity_key_attestation(
    &self,
    _request: &teamcraft_proto::PutIdentityKeyAttestationRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("put_identity_key_attestation")
  }

  fn list_role_evidence(
    &self,
  ) -> impl Future<Output = Result<teamcraft_proto::ListRoleEvidenceResponse, ApiError>> {
    self.inner.list_role_evidence()
  }

  fn list_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    query: &ListWorkspaceLogQuery,
  ) -> impl Future<Output = Result<ListWorkspaceLogResponse, ApiError>> {
    self.inner.list_workspace_log(kind, query)
  }

  fn append_workspace_log(
    &self,
    _kind: &WorkspaceLogKind,
    _request: &AppendWorkspaceLogRequest,
  ) -> impl Future<Output = Result<WorkspaceLogEntryResponse, ApiError>> {
    self.refused("append_workspace_log")
  }

  fn list_channel_role_log(
    &self,
    conversation: ConversationId,
    query: &teamcraft_proto::ListChannelRoleLogQuery,
  ) -> impl Future<Output = Result<teamcraft_proto::ListChannelRoleLogResponse, ApiError>> {
    self.inner.list_channel_role_log(conversation, query)
  }

  fn append_channel_role_log(
    &self,
    _conversation: ConversationId,
    _request: &teamcraft_proto::AppendChannelRoleLogRequest,
  ) -> impl Future<Output = Result<teamcraft_proto::ChannelRoleLogEntryResponse, ApiError>> {
    self.refused("append_channel_role_log")
  }

  fn create_attachment(
    &self,
    _request: &CreateAttachmentRequest,
  ) -> impl Future<Output = Result<CreateAttachmentResponse, ApiError>> {
    self.refused("create_attachment")
  }

  fn complete_attachment(
    &self,
    _attachment: AttachmentId,
  ) -> impl Future<Output = Result<AttachmentResponse, ApiError>> {
    self.refused("complete_attachment")
  }

  fn create_multipart_attachment(
    &self,
    _request: &CreateAttachmentRequest,
  ) -> impl Future<Output = Result<CreateMultipartAttachmentResponse, ApiError>> {
    self.refused("create_multipart_attachment")
  }

  fn presign_attachment_part(
    &self,
    _attachment: AttachmentId,
    _request: &PresignAttachmentPartRequest,
  ) -> impl Future<Output = Result<AttachmentPartUrlResponse, ApiError>> {
    self.refused("presign_attachment_part")
  }

  fn complete_multipart_attachment(
    &self,
    _attachment: AttachmentId,
    _request: &CompleteMultipartAttachmentRequest,
  ) -> impl Future<Output = Result<AttachmentResponse, ApiError>> {
    self.refused("complete_multipart_attachment")
  }

  fn abandon_attachment(&self, _attachment: AttachmentId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("abandon_attachment")
  }

  fn keep_attachment(&self, _attachment: AttachmentId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("keep_attachment")
  }

  fn download_attachment(
    &self,
    _attachment: AttachmentId,
  ) -> impl Future<Output = Result<AttachmentDownloadResponse, ApiError>> {
    self.refused("download_attachment")
  }

  fn create_background_upload(
    &self,
    _request: &CreateBackgroundUploadRequest,
  ) -> impl Future<Output = Result<CreateBackgroundUploadResponse, ApiError>> {
    self.refused("create_background_upload")
  }

  fn complete_background_upload(
    &self,
    _background: BackgroundId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("complete_background_upload")
  }

  fn download_background(
    &self,
    _background: BackgroundId,
  ) -> impl Future<Output = Result<BackgroundDownloadResponse, ApiError>> {
    self.refused("download_background")
  }

  fn delete_background(&self, _background: BackgroundId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("delete_background")
  }

  fn create_org_background_upload(
    &self,
    _request: &CreateOrgBackgroundUploadRequest,
  ) -> impl Future<Output = Result<CreateOrgBackgroundUploadResponse, ApiError>> {
    self.refused("create_org_background_upload")
  }

  fn complete_org_background_upload(
    &self,
    _background: OrgBackgroundId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("complete_org_background_upload")
  }

  fn download_org_background(
    &self,
    _background: OrgBackgroundId,
  ) -> impl Future<Output = Result<OrgBackgroundDownloadResponse, ApiError>> {
    self.refused("download_org_background")
  }

  fn delete_org_background(
    &self,
    _background: OrgBackgroundId,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("delete_org_background")
  }

  fn put_blob(&self, _url: &str, _bytes: Vec<u8>) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("put_blob")
  }

  fn put_blob_detached(&self, _url: &str, _bytes: Vec<u8>, _sent: ByteCounter) -> DetachedRequest {
    Box::pin(self.refused("put_blob_detached"))
  }

  fn put_part_detached(&self, _url: &str, _bytes: Vec<u8>, _sent: ByteCounter) -> DetachedPart {
    Box::pin(self.refused("put_part_detached"))
  }

  fn get_blob_detached(
    &self,
    _url: &str,
    _maybe_max_bytes: Option<u64>,
    _received: ByteCounter,
  ) -> DetachedDownload {
    Box::pin(self.refused("get_blob_detached"))
  }

  #[cfg(not(target_arch = "wasm32"))]
  fn get_blob_from_detached(&self, _url: &str, _start: u64) -> DetachedRange {
    Box::pin(self.refused("get_blob_from_detached"))
  }

  fn get_blob(&self, _url: &str) -> impl Future<Output = Result<Vec<u8>, ApiError>> {
    self.refused("get_blob")
  }

  fn get_blob_capped(&self, _url: &str, _max_bytes: u64) -> impl Future<Output = Result<Vec<u8>, ApiError>> {
    self.refused("get_blob_capped")
  }

  fn create_share_link(
    &self,
    _attachment: AttachmentId,
    _request: &CreateShareLinkRequest,
  ) -> impl Future<Output = Result<ShareLinkResponse, ApiError>> {
    self.refused("create_share_link")
  }

  fn revoke_share_link(&self, _share_id: &str) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("revoke_share_link")
  }

  fn list_share_links(&self) -> impl Future<Output = Result<ListShareLinksResponse, ApiError>> {
    self.inner.list_share_links()
  }

  fn register_push(
    &self,
    _device: DeviceId,
    _request: &RegisterPushRequest,
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("register_push")
  }

  fn unregister_push(&self, _device: DeviceId) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("unregister_push")
  }

  fn send_test_push(&self, _device: DeviceId) -> impl Future<Output = Result<TestPushResponse, ApiError>> {
    self.refused("send_test_push")
  }

  fn my_push_health(&self) -> impl Future<Output = Result<PushHealthResponse, ApiError>> {
    self.inner.my_push_health()
  }

  fn notification_settings(&self) -> impl Future<Output = Result<NotificationSettings, ApiError>> {
    self.inner.notification_settings()
  }

  fn push_gateway(&self) -> impl Future<Output = Result<PushGatewayResponse, ApiError>> {
    self.inner.push_gateway()
  }

  fn web_push_key(&self) -> impl Future<Output = Result<WebPushKeyResponse, ApiError>> {
    self.inner.web_push_key()
  }

  fn gateway_info(&self, _gateway_url: &str) -> impl Future<Output = Result<GatewayInfoResponse, ApiError>> {
    self.refused("gateway_info")
  }

  fn revoke_gateway_token(
    &self,
    _gateway_url: &str,
    _sealed_token: &[u8],
  ) -> impl Future<Output = Result<(), ApiError>> {
    self.refused("revoke_gateway_token")
  }

  fn connect_gateway(&self) -> impl Future<Output = Result<Self::Gateway, ApiError>> {
    self.refused("connect_gateway")
  }

  fn connect_rendezvous(&self) -> impl Future<Output = Result<Self::Rendezvous, ApiError>> {
    self.refused("connect_rendezvous")
  }

  fn gateway_token(&self) -> impl Future<Output = Result<String, ApiError>> {
    self.refused("gateway_token")
  }

  /// No tokens for the identity provider: nothing is signed or sent there.
  fn provider_tokens(&self) -> Option<ProviderTokens> {
    None
  }

  /// The proofs and nonces of the requests that are passed on.
  fn note_dpop_nonce(&self, nonce: &str) {
    self.inner.note_dpop_nonce(nonce);
  }
}
