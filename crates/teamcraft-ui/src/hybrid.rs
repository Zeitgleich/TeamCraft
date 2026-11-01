//! Composes the chat backend with the server's live state, the admin API and the calls.
//!
//! The chat (spaces, conversations, messages, search, own devices, keys) comes from `C`: the client
//! core over the desktop bridge or in the page, or the REST reader without a core. Live state
//! (unread counts, threads, presence, receipts, announcements) comes from the server, the
//! administration console from the admin API (admins and owners only), calls from the desktop shell
//! or, in the browser, from the page (`web_calls`).
//! Whatever is missing answers empty: there is no demo data here (it only exists in development
//! builds with the `demo` feature).

use leptos::prelude::{Get, GetUntracked, RwSignal, Set};
use std::rc::Rc;
use teamcraft_client_api::calls::{
  CallEndReason, CallMoveOffer, CallStatusSounds, CallsUnavailableReason, OrgBackgroundRule,
  RemoteVideoTrack, ScreenContentMode, ScreenShareResolution, VideoQuality,
};
use teamcraft_proto::OrgBackgroundId;

use teamcraft_proto::{
  AdminBrandsResponse, AdminCalendarSettingsResponse, AdminInstanceResponse, AdminMailSettingsResponse,
  AnnouncementResponse, BrandAssetSlot, ChannelModeration, CreateBrandRequest, PermissionSchemeResponse,
  Role, RuntimeSettingsKey, RuntimeSettingsResponse, RuntimeSettingsSection, SchemeChange,
  SendTestMailRequest, SetAnnouncementRequest, SettingsTestResponse, UpdateBrandRequest,
  UpdateCalendarSettingsRequest, UpdateMailSettingsRequest, WorkspacePolicy,
};

use teamcraft_client_api::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, FileInfo, LinkedMessage, MediaAnchor, MediaPage,
  MessageExtras, NewChannel, NewSpace, OutgoingMessage, PermalinkTarget, SavedItem, ShareLink,
  ShareLinkOptions, SpaceChanges, SpaceInvite,
};
use teamcraft_proto::{BackgroundId, CategoryId, HistoryPolicy, PresencePolicy};

use crate::admin::{
  AdminChannel, AdminLoad, AdminSpace, AdminSpaceChanges, AdminUser, AuditEntry, AuditFilter, DeviceInfo,
  PolicyBlocker, Retention, SelfJoinOutcome, SettingsTestKind,
};
use crate::admin_live::AdminLive;
use crate::auth::Authenticator;
use crate::backend::{AdminBackend, CallBackend, ChatBackend, SearchBackend, SecurityBackend};
use crate::calls::{
  ActiveCall, BackgroundEffect, BackgroundImageInfo, CallPreview, CallRequest, CameraFallback, EffectsOffer,
  HuddleInfo, IncomingCall, JoinMedia, MediaDevices, Participant, PreviewCamera,
};
use crate::clock::now_ms;
use crate::i18n::translate_current;
use crate::live::{LiveScope, LiveServer, REST_ONLY_POLL_INTERVAL_MS, thread_summary, with_local_expiry};
use crate::model::{
  Conversation, ConversationId, ConversationKind, CustomStatus, DeviceId, Message, MessageId, PresenceStatus,
  Space, SpaceId, ThreadId, ThreadSummary, TimestampMs, User, UserId, as_log_line,
};
use crate::reactive::{use_interval_while, use_page_visible};
use crate::remote_calls::RemoteCalls;
use crate::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};
use crate::security::KeyChange;
use crate::settings::{load_string, store_string};
use teamcraft_client_api::custom_status::shown_status;
use teamcraft_client_api::remote::CallAction;
use teamcraft_client_api::video_stats::VideoStatsJson;

pub struct Hybrid<C> {
  chat: Rc<C>,
  role: Role,
  maybe_live: Option<LiveServer>,
  /// The administration console on the admin API; only for admins and owners.
  maybe_admin: Option<AdminLive>,
  /// Calls (media in the desktop shell or in the page); without them the app offers no calls.
  maybe_calls: Option<RemoteCalls>,
  /// Why there are no calls, when there are none.
  calls_unavailable: CallsUnavailableReason,
}

impl<C: ChatBackend + 'static> Hybrid<C> {
  /// Calls, with the media in the desktop shell or in the page.
  #[must_use]
  pub fn with_calls(mut self, calls: RemoteCalls) -> Self {
    self.maybe_calls = Some(calls);
    self
  }

  /// No calls, for this reason.
  #[must_use]
  pub fn without_calls(mut self, reason: CallsUnavailableReason) -> Self {
    self.maybe_calls = None;
    self.calls_unavailable = reason;
    self
  }

  /// Chat plus the server's live state (unread counts, threads, presence, receipts).
  pub fn with_live(chat: C, role: Role, auth: Rc<dyn Authenticator>) -> Self {
    let chat = Rc::new(chat);
    let scope_chat = chat.clone();
    let scope = Rc::new(move || LiveScope {
      users: scope_chat.users().into_iter().map(|user| user.id).collect(),
    });
    let maybe_admin = (role >= Role::Admin).then(|| AdminLive::new(auth.clone()));
    Self {
      chat,
      role,
      maybe_live: Some(LiveServer::new(auth, scope)),
      maybe_admin,
      maybe_calls: None,
      calls_unavailable: CallsUnavailableReason::NotInThisApp,
    }
  }
}

/// A deleted account (issue #630) is shown as "Deleted user" in the UI's language wherever its
/// name appears (profile, members, messages); it has no username to show either.
fn named_if_deleted(mut user: User) -> User {
  if user.is_deleted {
    user.display_name = translate_current("user-deleted-name");
    user.username.clear();
  }
  user
}

impl<C: ChatBackend> Hybrid<C> {
  fn with_counts(&self, conversation: Conversation) -> Conversation {
    match &self.maybe_live {
      Some(live) => live.with_counts(conversation),
      None => conversation,
    }
  }

  /// The server knows who is online and what status they set; the chat only knows the roster. A
  /// status shows only while the workspace allows statuses and until it ended.
  fn with_live_presence(&self, mut user: User, policy: &PresencePolicy, now: TimestampMs) -> User {
    if let Some((presence, maybe_status)) = self
      .maybe_live
      .as_ref()
      .and_then(|live| live.presence_of(user.id))
    {
      user.presence = presence;
      user.maybe_custom_status = maybe_status;
    }
    user.maybe_custom_status = shown_status(policy, user.maybe_custom_status.take(), now);
    user
  }

  /// [`Self::with_live_presence`] for many people: the policy is read once.
  fn with_live_presences(&self, users: Vec<User>) -> Vec<User> {
    let policy = self.chat.presence_policy();
    let now = now_ms();
    users
      .into_iter()
      .map(|user| self.with_live_presence(user, &policy, now))
      .collect()
  }
}

impl<C: ChatBackend + 'static> ChatBackend for Hybrid<C> {
  fn set_change_notifier(&self, notify: Rc<dyn Fn()>) {
    if let Some(calls) = &self.maybe_calls {
      calls.set_change_notifier(notify.clone());
    }
    let Some(live) = &self.maybe_live else {
      self.chat.set_change_notifier(notify);
      return;
    };
    live.set_change_notifier(notify.clone());
    if let Some(admin) = &self.maybe_admin {
      admin.set_change_notifier(notify.clone());
    }
    // The core's snapshot carries the counters it keeps from gateway events; it says when the
    // rest must be read again (after a reconnect). Applied before the UI re-reads. The first
    // snapshot is taken before deciding about the poll, so a chat with a core never starts it.
    live.observe(self.chat.live_snapshot());
    // No core, so no gateway: polling is the only way to see new counts. Stops for good once a
    // snapshot shows a core (a shell that reports its live state only later).
    let is_rest_only = RwSignal::new(!live.is_core_fed());
    let observer = live.clone();
    // Weak: the chat keeps this notifier, which must not keep the chat alive after sign-out.
    let chat = Rc::downgrade(&self.chat);
    self.chat.set_change_notifier(Rc::new(move || {
      if let Some(chat) = chat.upgrade() {
        observer.observe(chat.live_snapshot());
      }
      if is_rest_only.try_get_untracked() == Some(true) && observer.is_core_fed() {
        is_rest_only.set(false);
      }
      notify();
    }));
    live.queue_refresh();
    // Paused while the page is hidden.
    let is_visible = use_page_visible();
    let poller = live.clone();
    use_interval_while(
      REST_ONLY_POLL_INTERVAL_MS,
      move || is_rest_only.get() && is_visible.get(),
      move || poller.queue_refresh(),
    );
  }

  fn meetings_revision(&self) -> u64 {
    self.chat.meetings_revision()
  }

  fn me(&self) -> User {
    self.with_live_presence(self.chat.me(), &self.chat.presence_policy(), now_ms())
  }

  fn user(&self, id: UserId) -> Option<User> {
    self
      .chat
      .user(id)
      .map(|user| self.with_live_presence(named_if_deleted(user), &self.chat.presence_policy(), now_ms()))
  }

  fn users(&self) -> Vec<User> {
    self.with_live_presences(self.chat.users())
  }

  fn search_people(&self, query: &str) -> Vec<User> {
    self.with_live_presences(self.chat.search_people(query))
  }

  fn spaces(&self) -> Vec<Space> {
    self.chat.spaces()
  }

  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation> {
    self
      .chat
      .space_conversations(space)
      .into_iter()
      .map(|conversation| self.with_counts(conversation))
      .collect()
  }

  fn direct_conversations(&self) -> Vec<Conversation> {
    self
      .chat
      .direct_conversations()
      .into_iter()
      .map(|conversation| self.with_counts(conversation))
      .collect()
  }

  fn conversation(&self, id: ConversationId) -> Option<Conversation> {
    self
      .chat
      .conversation(id)
      .map(|conversation| self.with_counts(conversation))
  }

  fn message_count(&self, conversation: ConversationId) -> usize {
    self.chat.message_count(conversation)
  }

  fn prefetch_messages(&self, conversation: ConversationId, start: usize, len: usize) {
    self.chat.prefetch_messages(conversation, start, len);
  }

  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message> {
    let messages = self.chat.messages(conversation, start, len);
    // Integration channels are fed by bridges that write `[LEVEL] text`; show them as log lines.
    let is_log = self
      .chat
      .conversation(conversation)
      .is_some_and(|conversation| conversation.kind == ConversationKind::Integration);
    if is_log {
      messages.into_iter().map(as_log_line).collect()
    } else {
      messages
    }
  }

  fn thread(&self, conversation: ConversationId, thread: ThreadId) -> Vec<Message> {
    self.chat.thread(conversation, thread)
  }

  fn followed_threads(&self) -> Vec<ThreadSummary> {
    let Some(live) = &self.maybe_live else {
      return self.chat.followed_threads();
    };
    let mut summaries: Vec<ThreadSummary> = live
      .thread_entries()
      .iter()
      .filter_map(|entry| thread_summary(entry, &self.chat.thread(entry.conversation_id, entry.thread_id)))
      .collect();
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last_reply_at));
    summaries
  }

  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.chat.post_message(conversation, maybe_thread, body);
  }

  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str) {
    self.chat.toggle_reaction(conversation, message, emoji);
  }

  fn mark_read(&self, conversation: ConversationId) {
    if let Some(live) = &self.maybe_live {
      live.clear_unread(conversation);
    }
    self.chat.mark_read(conversation);
  }

  fn mark_read_through(&self, conversation: ConversationId, message: MessageId) {
    self.chat.mark_read_through(conversation, message);
  }

  fn set_bookmarks(
    &self,
    conversation: ConversationId,
    bookmarks: Vec<teamcraft_client_api::model::Bookmark>,
  ) {
    self.chat.set_bookmarks(conversation, bookmarks);
  }

  fn remove_member(&self, conversation: ConversationId, user: UserId) {
    self.chat.remove_member(conversation, user);
  }

  fn open_dm(&self, user: UserId) {
    self.chat.open_dm(user);
  }

  fn open_group_dm(&self, users: Vec<UserId>) {
    self.chat.open_group_dm(users);
  }

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    self.chat.set_favorite(conversation, is_favorite);
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.chat.set_presence(presence);
  }

  fn set_presence_until(&self, presence: PresenceStatus, maybe_until: Option<TimestampMs>) {
    self.chat.set_presence_until(presence, maybe_until);
  }

  /// The end is resolved here, where the user's time zone is known. With a client core the core
  /// queues the change (offline, retries); without one it goes over REST.
  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    let now = now_ms();
    let maybe_status = maybe_status.map(|status| with_local_expiry(status, now));
    match &self.maybe_live {
      Some(live) if live.is_core_fed() => {
        let me = self.chat.me().id;
        live.show_own_status(me, maybe_status.as_ref().and_then(|status| status.to_wire(now)));
        self.chat.set_custom_status(maybe_status);
      },
      Some(live) => live.set_custom_status(self.chat.me().id, maybe_status.as_ref(), now),
      None => self.chat.set_custom_status(maybe_status),
    }
  }

  fn presence_policy(&self) -> PresencePolicy {
    self.chat.presence_policy()
  }

  fn screen_share_policy(&self) -> teamcraft_proto::ScreenSharePolicy {
    self.chat.screen_share_policy()
  }

  fn set_activity(&self, is_idle: bool) {
    self.chat.set_activity(is_idle);
  }

  fn focus_conversation(&self, conversation: ConversationId) {
    self.chat.focus_conversation(conversation);
  }

  fn set_attention(&self, is_attentive: bool) {
    self.chat.set_attention(is_attentive);
  }

  fn search_backend(&self) -> Option<&dyn SearchBackend> {
    self.chat.search_backend()
  }

  fn send_message(&self, message: OutgoingMessage) {
    self.chat.send_message(message);
  }

  fn message_extras(&self, conversation: ConversationId, messages: &[MessageId]) -> Vec<MessageExtras> {
    self.chat.message_extras(conversation, messages)
  }

  fn edit_message(&self, conversation: ConversationId, message: MessageId, body: &str) {
    self.chat.edit_message(conversation, message, body);
  }

  fn delete_message(&self, conversation: ConversationId, message: MessageId) {
    self.chat.delete_message(conversation, message);
  }

  fn forward_message(&self, from: ConversationId, message: MessageId, to: ConversationId, comment: &str) {
    self.chat.forward_message(from, message, to, comment);
  }

  fn set_pinned(&self, conversation: ConversationId, message: MessageId, is_pinned: bool) {
    self.chat.set_pinned(conversation, message, is_pinned);
  }

  fn pinned_messages(&self, conversation: ConversationId) -> Vec<Message> {
    self.chat.pinned_messages(conversation)
  }

  fn set_saved(&self, conversation: ConversationId, message: MessageId, is_saved: bool) {
    self.chat.set_saved(conversation, message, is_saved);
  }

  fn saved_items(&self) -> Vec<SavedItem> {
    self.chat.saved_items()
  }

  fn background_images(&self) -> Vec<BackgroundImageInfo> {
    self.chat.background_images()
  }

  fn add_background_image(&self, name: &str, picture: Vec<u8>) {
    self.chat.add_background_image(name, picture);
  }

  fn delete_background_image(&self, id: BackgroundId) {
    self.chat.delete_background_image(id);
  }

  fn add_org_background(&self, name: &str, picture: Vec<u8>) {
    self.chat.add_org_background(name, picture);
  }

  fn delete_org_background(&self, id: OrgBackgroundId) {
    self.chat.delete_org_background(id);
  }

  fn set_org_background_rule(&self, rule: OrgBackgroundRule) {
    self.chat.set_org_background_rule(rule);
  }

  fn permalink(&self, conversation: ConversationId, message: MessageId) -> Option<String> {
    self.chat.permalink(conversation, message)
  }

  fn resolve_permalink(&self, link: &str) -> Option<PermalinkTarget> {
    self.chat.resolve_permalink(link)
  }

  fn linked_message(&self, link: &str) -> Option<LinkedMessage> {
    self.chat.linked_message(link)
  }

  fn mark_unread(&self, conversation: ConversationId, from: MessageId) {
    self.chat.mark_unread(conversation, from);
  }

  fn first_unread(&self, conversation: ConversationId) -> Option<MessageId> {
    self.chat.first_unread(conversation)
  }

  fn newest_message(&self, conversation: ConversationId) -> Option<MessageId> {
    self.chat.newest_message(conversation)
  }

  fn is_waiting_for_device(&self, conversation: ConversationId) -> bool {
    self.chat.is_waiting_for_device(conversation)
  }

  fn membership_issues(
    &self,
    conversation: ConversationId,
  ) -> Vec<teamcraft_client_api::model::MembershipIssue> {
    self.chat.membership_issues(conversation)
  }

  fn retry_removal(&self, conversation: ConversationId, user: UserId) {
    self.chat.retry_removal(conversation, user);
  }

  fn retry_dm_setup(&self, conversation: ConversationId) {
    self.chat.retry_dm_setup(conversation);
  }

  fn set_up_meeting_chat(&self, conversation: ConversationId, invitees: Vec<UserId>) {
    self.chat.set_up_meeting_chat(conversation, invitees);
  }

  fn update_meeting_chat_members(
    &self,
    conversation: ConversationId,
    invitees: Vec<UserId>,
    removed: Vec<UserId>,
  ) {
    self
      .chat
      .update_meeting_chat_members(conversation, invitees, removed);
  }

  fn add_people_to_call(
    &self,
    from: ConversationId,
    in_call: Vec<UserId>,
    added: Vec<UserId>,
    maybe_title: Option<String>,
    key: teamcraft_proto::IdempotencyKey,
  ) {
    self
      .chat
      .add_people_to_call(from, in_call, added, maybe_title, key);
  }

  fn let_into_meeting_chat(&self, conversation: ConversationId, user: UserId) {
    self.chat.let_into_meeting_chat(conversation, user);
  }

  fn prepare_guest_link(&self, conversation: ConversationId, meeting: teamcraft_proto::MeetingId) {
    self.chat.prepare_guest_link(conversation, meeting);
  }

  fn guest_link_fragment(&self, meeting: teamcraft_proto::MeetingId) -> Option<String> {
    self.chat.guest_link_fragment(meeting)
  }

  fn let_guest_into_meeting_chat(
    &self,
    conversation: ConversationId,
    guest: UserId,
    knock_fingerprint: String,
    access_ends_at_ms: i64,
  ) {
    self
      .chat
      .let_guest_into_meeting_chat(conversation, guest, knock_fingerprint, access_ends_at_ms);
  }

  fn prepare_guest_key_code(&self, guest: UserId) {
    self.chat.prepare_guest_key_code(guest);
  }

  fn guest_key_code(&self, guest: UserId) -> Option<String> {
    self.chat.guest_key_code(guest)
  }

  fn note_meeting_lobby(&self, meeting: teamcraft_proto::MeetingId, has_lobby: bool) {
    self.chat.note_meeting_lobby(meeting, has_lobby);
  }

  fn is_open_by_own_record(&self, meeting: teamcraft_proto::MeetingId) -> bool {
    self.chat.is_open_by_own_record(meeting)
  }

  fn discard_removal(&self, conversation: ConversationId, user: UserId) {
    self.chat.discard_removal(conversation, user);
  }

  fn unconfirmed_removals(&self) -> Vec<(ConversationId, UserId)> {
    self.chat.unconfirmed_removals()
  }

  fn group_leftovers(&self) -> Vec<(ConversationId, UserId)> {
    self.chat.group_leftovers()
  }

  fn draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) -> Option<String> {
    self.chat.draft(conversation, maybe_thread)
  }

  fn set_draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.chat.set_draft(conversation, maybe_thread, body);
  }

  fn retry_send(&self, conversation: ConversationId, message: MessageId) {
    self.chat.retry_send(conversation, message);
  }

  fn discard_unsent(&self, conversation: ConversationId, message: MessageId) {
    self.chat.discard_unsent(conversation, message);
  }

  fn cancel_upload(&self, conversation: ConversationId, message: MessageId, file_id: &str) {
    self.chat.cancel_upload(conversation, message, file_id);
  }

  fn acknowledge(&self, conversation: ConversationId, message: MessageId) {
    self.chat.acknowledge(conversation, message);
  }

  fn conversation_files(&self, conversation: ConversationId) -> Vec<FileInfo> {
    self.chat.conversation_files(conversation)
  }

  fn conversation_media(
    &self,
    conversation: ConversationId,
    anchor: MediaAnchor,
    max_messages: usize,
  ) -> Option<MediaPage> {
    self.chat.conversation_media(conversation, anchor, max_messages)
  }

  fn download_file(&self, conversation: ConversationId, message: MessageId) {
    self.chat.download_file(conversation, message);
  }

  fn create_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    options: ShareLinkOptions,
  ) {
    self
      .chat
      .create_share_link(conversation, message, file_id, options);
  }

  fn share_links(&self, conversation: ConversationId, message: MessageId, file_id: &str) -> Vec<ShareLink> {
    self.chat.share_links(conversation, message, file_id)
  }
  fn revoke_share_link(
    &self,
    conversation: ConversationId,
    message: MessageId,
    file_id: &str,
    share_id: &str,
  ) {
    self
      .chat
      .revoke_share_link(conversation, message, file_id, share_id);
  }
  fn refresh_share_links(&self, conversation: ConversationId, message: MessageId, file_id: &str) {
    self.chat.refresh_share_links(conversation, message, file_id);
  }

  fn max_upload_bytes(&self) -> Option<u64> {
    self.chat.max_upload_bytes()
  }

  fn create_space(&self, space: NewSpace) {
    self.chat.create_space(space);
  }

  fn update_space(&self, space: SpaceId, changes: SpaceChanges) {
    self.chat.update_space(space, changes);
  }

  fn create_channel_with(&self, channel: NewChannel) {
    self.chat.create_channel_with(channel);
  }

  fn update_channel(&self, conversation: ConversationId, changes: ChannelChanges) {
    self.chat.update_channel(conversation, changes);
  }

  fn browse_channels(&self, space: SpaceId) -> Vec<BrowsableChannel> {
    self.chat.browse_channels(space)
  }

  fn join_channel(&self, conversation: ConversationId) {
    self.chat.join_channel(conversation);
  }

  fn joinable_spaces(&self) -> Vec<teamcraft_client_api::model::JoinableSpace> {
    self.chat.joinable_spaces()
  }

  fn join_space(&self, space: SpaceId) {
    self.chat.join_space(space);
  }

  fn leave_channel(&self, conversation: ConversationId) {
    self.chat.leave_channel(conversation);
  }

  fn categories(&self, space: SpaceId) -> Vec<CategoryInfo> {
    self.chat.categories(space)
  }

  fn create_category(&self, space: SpaceId, name: &str) {
    self.chat.create_category(space, name);
  }

  fn rename_category(&self, space: SpaceId, category: CategoryId, name: &str) {
    self.chat.rename_category(space, category, name);
  }

  fn delete_category(&self, space: SpaceId, category: CategoryId) {
    self.chat.delete_category(space, category);
  }

  fn add_member_with_history(&self, conversation: ConversationId, user: UserId, history: HistoryPolicy) {
    self.chat.add_member_with_history(conversation, user, history);
  }

  fn invite_to_space(&self, invite: SpaceInvite) {
    self.chat.invite_to_space(invite);
  }

  fn space_members(&self, space: SpaceId) -> teamcraft_client_api::model::SpaceRoster {
    self.chat.space_members(space)
  }

  fn integration_sources(
    &self,
    conversation: ConversationId,
  ) -> Vec<teamcraft_client_api::model::IntegrationSource> {
    self.chat.integration_sources(conversation)
  }

  fn can_manage_integration_sources(&self) -> bool {
    self.chat.can_manage_integration_sources()
  }

  fn add_integration_source(
    &self,
    conversation: ConversationId,
    source: teamcraft_client_api::model::NewIntegrationSource,
  ) {
    self.chat.add_integration_source(conversation, source);
  }

  fn revoke_integration_source(&self, conversation: ConversationId, source: teamcraft_proto::SourceId) {
    self.chat.revoke_integration_source(conversation, source);
  }

  fn notetaker_policy(&self) -> Option<teamcraft_proto::NotetakerPolicy> {
    self.chat.notetaker_policy()
  }

  fn request_notetaker(&self, conversation: ConversationId, request: teamcraft_proto::NotetakerRequest) {
    self.chat.request_notetaker(conversation, request);
  }

  fn channel_roles(&self, conversation: ConversationId) -> Vec<teamcraft_client_api::model::ChannelMember> {
    self.chat.channel_roles(conversation)
  }

  fn can_sign_channel_roles(&self, conversation: ConversationId) -> bool {
    self.chat.can_sign_channel_roles(conversation)
  }

  fn confirm_channel_role(&self, conversation: ConversationId, user: UserId) {
    self.chat.confirm_channel_role(conversation, user);
  }

  fn refresh_channel_roles(&self, conversation: ConversationId) {
    self.chat.refresh_channel_roles(conversation);
  }

  fn set_channel_role(
    &self,
    conversation: ConversationId,
    user: UserId,
    role: teamcraft_client_api::model::ChannelRole,
  ) {
    self.chat.set_channel_role(conversation, user, role);
  }

  fn space_invites(&self, space: SpaceId) -> Vec<teamcraft_client_api::model::PendingInvite> {
    self.chat.space_invites(space)
  }

  fn add_people_to_channel(
    &self,
    conversation: ConversationId,
    people: Vec<(UserId, bool)>,
    history: HistoryPolicy,
    joins_space: bool,
  ) {
    self
      .chat
      .add_people_to_channel(conversation, people, history, joins_space);
  }

  fn refresh_space_roster(&self, space: SpaceId) {
    self.chat.refresh_space_roster(space);
  }

  fn set_space_role(&self, space: SpaceId, user: UserId, role: teamcraft_client_api::model::SpaceRole) {
    self.chat.set_space_role(space, user, role);
  }

  fn remove_space_member(&self, space: SpaceId, user: UserId) {
    self.chat.remove_space_member(space, user);
  }

  fn revoke_space_invite(&self, space: SpaceId, invite: teamcraft_client_api::model::InviteId) {
    self.chat.revoke_space_invite(space, invite);
  }

  fn add_space_members(&self, space: SpaceId, users: Vec<UserId>, is_guest: bool) {
    self.chat.add_space_members(space, users, is_guest);
  }

  fn call_move_offer(&self, from: ConversationId, to: ConversationId) -> Option<CallMoveOffer> {
    self.chat.call_move_offer(from, to)
  }

  fn accept_call_move(&self, from: ConversationId, to: ConversationId) {
    self.chat.accept_call_move(from, to);
  }

  fn has_external_people(&self, conversation: ConversationId) -> bool {
    self.chat.has_external_people(conversation)
  }

  fn activity(&self) -> teamcraft_client_api::activity::ActivityFeed {
    self.chat.activity()
  }

  fn join_requests(&self) -> Vec<teamcraft_client_api::activity::JoinRequest> {
    self.chat.join_requests()
  }

  fn mark_activity_read(&self) {
    self.chat.mark_activity_read();
  }

  fn last_seen(&self, user: UserId) -> Option<TimestampMs> {
    self.chat.last_seen(user)
  }

  fn privacy(&self) -> teamcraft_client_api::privacy::PrivacySettings {
    self.chat.privacy()
  }

  fn set_last_seen_visibility(&self, visibility: teamcraft_client_api::privacy::LastSeenVisibility) {
    self.chat.set_last_seen_visibility(visibility);
  }

  fn call_history(&self) -> Vec<teamcraft_client_api::calls::CallHistoryItem> {
    self.chat.call_history()
  }

  fn refresh_call_history(&self) {
    self.chat.refresh_call_history();
  }

  fn is_loaded(&self, list: teamcraft_client_api::backend::LoadingList) -> bool {
    self.chat.is_loaded(list)
  }

  fn is_in_call(&self, user: UserId) -> bool {
    self
      .maybe_calls
      .as_ref()
      .is_some_and(|calls| calls.is_in_call(user))
  }

  fn is_thread_followed(&self, conversation: ConversationId, thread: ThreadId) -> bool {
    self.maybe_live.as_ref().is_none_or(|live| {
      live
        .thread_entries()
        .iter()
        .any(|entry| entry.conversation_id == conversation && entry.thread_id == thread)
    })
  }

  fn set_thread_followed(&self, conversation: ConversationId, thread: ThreadId, is_followed: bool) {
    if let Some(live) = &self.maybe_live {
      live.set_thread_followed(conversation, thread, is_followed);
    }
  }

  fn mark_thread_read(&self, conversation: ConversationId, thread: ThreadId) {
    let (Some(live), Some(latest)) = (
      &self.maybe_live,
      self
        .chat
        .thread(conversation, thread)
        .iter()
        // The root opens the list but isn't a reply; the server only accepts replies.
        .rfind(|message| message.maybe_thread == Some(thread))
        .map(|message| message.id),
    ) else {
      return;
    };
    live.mark_thread_read(conversation, thread, latest);
  }

  fn push_content_mode(&self) -> teamcraft_proto::PushContentMode {
    self.chat.push_content_mode()
  }

  fn owner_roster(&self) -> Option<teamcraft_client_api::admin::OwnerRosterStatus> {
    self.chat.owner_roster()
  }

  fn can_sign_workspace(&self) -> bool {
    self.chat.can_sign_workspace()
  }

  fn admin_add_self(&self, conversation: ConversationId) {
    self.chat.admin_add_self(conversation);
  }

  fn request_admin_join(&self, conversation: ConversationId) {
    self.chat.request_admin_join(conversation);
  }

  fn withdraw_admin_join(&self, conversation: ConversationId) {
    self.chat.withdraw_admin_join(conversation);
  }

  fn approve_admin_join(&self, conversation: ConversationId, user: UserId, history: HistoryPolicy) {
    self.chat.approve_admin_join(conversation, user, history);
  }

  fn decline_admin_join(&self, conversation: ConversationId, user: UserId) {
    self.chat.decline_admin_join(conversation, user);
  }

  fn current_announcement(&self) -> Option<AnnouncementResponse> {
    self.maybe_live.as_ref().and_then(LiveServer::announcement)
  }

  fn dismiss_announcement(&self, id: teamcraft_proto::AnnouncementId) {
    if let Some(live) = &self.maybe_live {
      live.dismiss_announcement(id);
    }
  }

  fn link_request_mark(&self) -> u64 {
    self.chat.link_request_mark()
  }

  fn seen_by(&self, conversation: ConversationId, message: MessageId) -> Vec<UserId> {
    self
      .maybe_live
      .as_ref()
      .map_or_else(Vec::new, |live| live.seen_by(conversation, message))
  }

  fn typing_users(&self, conversation: ConversationId) -> Vec<UserId> {
    self.chat.typing_users(conversation)
  }

  fn typing_now(&self) -> Vec<crate::model::TypingNow> {
    self.chat.typing_now()
  }

  fn notify_typing(&self, conversation: ConversationId, maybe_thread: Option<crate::model::ThreadId>) {
    self.chat.notify_typing(conversation, maybe_thread);
  }
}

impl<C: ChatBackend> SearchBackend for Hybrid<C> {
  /// Search needs decrypted messages: the client core answers from its local index. Without a core
  /// (the REST reader) there is nothing to search.
  fn search(&self, query: &ParsedQuery) -> SearchResults {
    self
      .chat
      .search_backend()
      .map_or_else(SearchResults::default, |real| real.search(query))
  }

  fn recent_mentions(&self) -> Vec<SearchHit> {
    self
      .chat
      .search_backend()
      .map_or_else(Vec::new, SearchBackend::recent_mentions)
  }

  fn saved_searches(&self) -> Vec<SavedSearch> {
    self
      .chat
      .search_backend()
      .map_or_else(Vec::new, SearchBackend::saved_searches)
  }

  fn save_search(&self, name: &str, query: &str) {
    if let Some(real) = self.chat.search_backend() {
      real.save_search(name, query);
    }
  }

  fn delete_saved_search(&self, id: u64) {
    if let Some(real) = self.chat.search_backend() {
      real.delete_saved_search(id);
    }
  }
}

impl<C: ChatBackend> CallBackend for Hybrid<C> {
  fn media_devices(&self) -> MediaDevices {
    self
      .maybe_calls
      .as_ref()
      .map_or_else(MediaDevices::default, RemoteCalls::devices)
  }

  fn call_participants(&self, conversation: ConversationId) -> Vec<Participant> {
    let Some(calls) = &self.maybe_calls else {
      return Vec::new();
    };
    let me = self.chat.me().id;
    calls
      .participants(conversation)
      .into_iter()
      .filter(|participant| participant.user != me)
      .collect()
  }

  fn has_calls(&self) -> bool {
    self.maybe_calls.is_some()
  }

  fn calls_unavailable_reason(&self) -> CallsUnavailableReason {
    self.calls_unavailable
  }

  fn effects_offer(&self) -> EffectsOffer {
    self
      .maybe_calls
      .as_ref()
      .map_or(EffectsOffer::Full, RemoteCalls::effects_offer)
  }

  fn call_join_failures(&self) -> u64 {
    self.maybe_calls.as_ref().map_or(0, RemoteCalls::join_failures)
  }

  fn has_pending_call_actions(&self) -> bool {
    self
      .maybe_calls
      .as_ref()
      .is_some_and(RemoteCalls::has_pending_actions)
  }

  fn call_video_tracks(&self) -> Vec<RemoteVideoTrack> {
    self
      .maybe_calls
      .as_ref()
      .map_or_else(Vec::new, RemoteCalls::video_tracks)
  }

  fn fetch_call_video_stats(&self, done: Box<dyn FnOnce(Option<VideoStatsJson>)>) {
    match &self.maybe_calls {
      Some(calls) => calls.fetch_video_stats(done),
      None => done(None),
    }
  }

  fn huddle(&self, conversation: ConversationId) -> Option<HuddleInfo> {
    self
      .maybe_calls
      .as_ref()
      .and_then(|calls| calls.huddle(conversation))
  }

  fn active_call(&self) -> Option<ActiveCall> {
    self.maybe_calls.as_ref().and_then(RemoteCalls::active)
  }

  fn call_end_reason(&self) -> Option<CallEndReason> {
    self.maybe_calls.as_ref().and_then(RemoteCalls::end_reason)
  }

  fn incoming_calls(&self) -> Vec<IncomingCall> {
    self
      .maybe_calls
      .as_ref()
      .map_or_else(Vec::new, RemoteCalls::incoming)
  }

  fn join_call(&self, request: CallRequest, media: JoinMedia) {
    if let Some(calls) = &self.maybe_calls {
      calls.join(request, media);
    }
  }

  fn leave_call(&self) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::Leave);
    }
  }

  fn decline_call(&self, conversation: ConversationId) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::Decline { conversation });
    }
  }

  fn set_call_muted(&self, is_muted: bool) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SetMuted { is_muted });
    }
  }

  fn set_call_deafened(&self, is_deafened: bool) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SetDeafened { is_deafened });
    }
  }

  fn move_call_here(&self) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::MoveHere);
    }
  }

  fn leave_call_everywhere(&self, conversation: ConversationId) {
    if let Some(live) = &self.maybe_live {
      live.leave_call_everywhere(conversation);
    }
  }

  fn select_microphone(&self, maybe_device: Option<String>) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SelectMicrophone { maybe_device });
    }
  }

  fn select_speakers(&self, maybe_device: Option<String>) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SelectSpeakers { maybe_device });
    }
  }

  fn refresh_media_devices(&self) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::RefreshDevices);
    }
  }

  fn test_speakers(&self, maybe_device: Option<String>) {
    if let Some(calls) = &self.maybe_calls {
      calls.test_speakers(maybe_device);
    }
  }

  fn set_call_camera(&self, is_on: bool) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SetCamera { is_on });
    }
  }

  fn select_camera(&self, maybe_device: Option<String>) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SelectCamera { maybe_device });
    }
  }

  fn camera_fallback(&self) -> Option<CameraFallback> {
    self.maybe_calls.as_ref().and_then(RemoteCalls::camera_fallback)
  }

  fn set_background_effect(&self, effect: BackgroundEffect) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SetBackgroundEffect { effect });
    }
  }

  fn start_screen_share(
    &self,
    has_sound: bool,
    content_mode: ScreenContentMode,
    resolution: ScreenShareResolution,
  ) {
    if let Some(calls) = &self.maybe_calls {
      calls.start_screen_share(has_sound, content_mode, resolution);
    }
  }

  fn stop_screen_share(&self) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::StopScreenShare);
    }
  }

  fn open_system_video_effects(&self) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::OpenSystemVideoEffects);
    }
  }

  fn set_call_status_sounds(&self, sounds: CallStatusSounds) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SetStatusSounds { sounds });
    }
  }

  fn set_camera_quality(&self, quality: VideoQuality) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SetCameraQuality { quality });
    }
  }

  fn set_video_quality(&self, stream_id: teamcraft_proto::MediaStreamId, quality: VideoQuality) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SetVideoQuality { stream_id, quality });
    }
  }

  fn set_call_preview(&self, preview: CallPreview) {
    if let Some(calls) = &self.maybe_calls {
      calls.set_preview(preview);
    }
  }

  fn call_preview_camera(&self) -> PreviewCamera {
    self
      .maybe_calls
      .as_ref()
      .map_or(PreviewCamera::Off, RemoteCalls::preview_camera)
  }

  fn call_preview_level(&self) -> f32 {
    self.maybe_calls.as_ref().map_or(0.0, RemoteCalls::preview_level)
  }

  fn watch_call_level(&self, is_watching: bool) {
    if let Some(calls) = &self.maybe_calls {
      calls.watch_call_level(is_watching);
    }
  }
}

impl<C: ChatBackend> AdminBackend for Hybrid<C> {
  fn my_role(&self) -> Role {
    self.role.clone()
  }

  fn admin_channels(&self) -> Vec<AdminChannel> {
    match &self.maybe_admin {
      Some(admin) => admin.channels(),
      None => Vec::new(),
    }
  }

  fn admin_users(&self) -> Vec<AdminUser> {
    match &self.maybe_admin {
      Some(admin) => admin.users(),
      None => Vec::new(),
    }
  }

  fn workspace_policy(&self) -> (u64, WorkspacePolicy) {
    match &self.maybe_admin {
      Some(admin) => (admin.policy_version(), admin.policy()),
      None => (0, WorkspacePolicy::default()),
    }
  }

  fn set_workspace_policy(
    &self,
    policy: WorkspacePolicy,
    genesis: Vec<teamcraft_client_api::admin::RosterEntry>,
  ) {
    let Some(admin) = &self.maybe_admin else {
      return;
    };
    // Only the roster the owner confirmed with IDs and keys, never the server's role holders as
    // they come (#265).
    let (owners, admins) = teamcraft_client_api::admin::roster_by_role(&genesis);
    // The editor started from what the server showed; the core signs only if that is also what it
    // verified itself (a compromised server could show altered fields).
    self.chat.publish_policy(admin.policy(), policy, owners, admins);
    admin.queue_refresh_after_publish();
  }

  fn admin_spaces(&self) -> Vec<AdminSpace> {
    match &self.maybe_admin {
      Some(admin) => admin.spaces(),
      None => Vec::new(),
    }
  }

  fn admin_load(&self) -> AdminLoad {
    self
      .maybe_admin
      .as_ref()
      .map_or(AdminLoad::Loaded, AdminLive::list_load)
  }

  fn change_admin_space(&self, space: SpaceId, changes: AdminSpaceChanges) {
    if let Some(admin) = &self.maybe_admin {
      admin.change_space(space, changes);
    }
  }

  fn permission_scheme(&self, maybe_space: Option<SpaceId>) -> PermissionSchemeResponse {
    match &self.maybe_admin {
      Some(admin) => admin.permission_scheme(maybe_space),
      None => PermissionSchemeResponse { roles: Vec::new() },
    }
  }

  fn change_permissions(&self, maybe_space: Option<SpaceId>, changes: Vec<SchemeChange>) {
    if let Some(admin) = &self.maybe_admin {
      admin.change_permissions(maybe_space, changes);
    }
  }

  fn channel_moderation(&self, channel: ConversationId) -> ChannelModeration {
    match &self.maybe_admin {
      Some(admin) => admin.channel_moderation(channel),
      None => ChannelModeration::default(),
    }
  }

  fn set_channel_moderation(&self, channel: ConversationId, moderation: ChannelModeration) {
    if let Some(admin) = &self.maybe_admin {
      admin.set_channel_moderation(channel, moderation);
    }
  }

  fn channel_member_candidates(&self, channel: ConversationId) -> Vec<UserId> {
    match &self.maybe_admin {
      Some(admin) => admin.channel_member_candidates(channel),
      None => Vec::new(),
    }
  }

  fn assign_channel_owner(&self, channel: ConversationId, user: UserId) {
    if let Some(admin) = &self.maybe_admin {
      admin.assign_channel_owner(channel, user);
    }
  }

  fn announcement(&self) -> Option<AnnouncementResponse> {
    match &self.maybe_admin {
      Some(admin) => admin.announcement(),
      None => None,
    }
  }

  fn set_announcement(&self, announcement: SetAnnouncementRequest) {
    if let Some(admin) = &self.maybe_admin {
      admin.set_announcement(announcement);
    }
  }

  fn clear_announcement(&self) {
    if let Some(admin) = &self.maybe_admin {
      admin.clear_announcement();
    }
  }

  fn instance_status(&self) -> Option<AdminInstanceResponse> {
    match &self.maybe_admin {
      Some(admin) => admin.instance(),
      None => None,
    }
  }

  fn audit_filter(&self) -> AuditFilter {
    match &self.maybe_admin {
      Some(admin) => admin.audit_filter(),
      None => AuditFilter::default(),
    }
  }

  fn set_audit_filter(&self, filter: AuditFilter) {
    if let Some(admin) = &self.maybe_admin {
      admin.set_audit_filter(filter);
    }
  }

  fn set_channel_retention(&self, channel: ConversationId, retention: Retention) {
    if let Some(admin) = &self.maybe_admin {
      admin.set_channel_retention(channel, retention);
    }
  }

  fn remove_device(&self, user: UserId, device: DeviceId) {
    if let Some(admin) = &self.maybe_admin {
      admin.remove_device(user, device);
    }
  }

  fn set_user_active(&self, user: UserId, is_active: bool) {
    if let Some(admin) = &self.maybe_admin {
      admin.set_user_active(user, is_active);
    }
  }

  fn remove_user_from_chats(&self, user: UserId) {
    // Signed by this client, never the server's removal alone (security review A8).
    if self.can_remove_from_chats() {
      self.chat.admin_remove_from_all_chats(user);
      if let Some(admin) = &self.maybe_admin {
        admin.queue_refresh_after_publish();
      }
    }
  }

  fn can_remove_from_chats(&self) -> bool {
    self.chat.can_sign_workspace()
  }

  fn delete_user(&self, user: UserId) {
    // The signed removals come first, in the client (security review A8).
    if self.can_remove_from_chats() {
      self.chat.admin_delete_user(user);
      if let Some(admin) = &self.maybe_admin {
        admin.queue_refresh_after_publish();
      }
    }
  }

  fn remove_group_leftover(&self, conversation: ConversationId, user: UserId) {
    if self.can_remove_from_chats() {
      self.chat.admin_remove_group_leftover(conversation, user);
    }
  }

  /// Organization recovery needs the recovery key ceremony of the core, which the console does not
  /// drive yet; `can_start_org_recovery` keeps the button disabled.
  fn start_org_recovery(&self, _user: UserId) {}

  fn admin_join_channel(&self, channel: ConversationId) -> SelfJoinOutcome {
    let Some(admin) = &self.maybe_admin else {
      return SelfJoinOutcome::Disabled;
    };
    match admin.policy().admin_self_join {
      teamcraft_proto::AdminSelfJoin::Visible => {
        self.chat.admin_add_self(channel);
        SelfJoinOutcome::Joined
      },
      // The channel's owners decide (admin.md §2.4).
      teamcraft_proto::AdminSelfJoin::OwnerApproval => {
        self.chat.request_admin_join(channel);
        SelfJoinOutcome::ApprovalRequested
      },
      teamcraft_proto::AdminSelfJoin::Disabled | teamcraft_proto::AdminSelfJoin::Unknown(_) => {
        SelfJoinOutcome::Disabled
      },
    }
  }

  fn audit_log(&self) -> Vec<AuditEntry> {
    match &self.maybe_admin {
      Some(admin) => admin.audit(),
      None => Vec::new(),
    }
  }

  fn admin_refresh(&self) {
    if let Some(admin) = &self.maybe_admin {
      admin.queue_refresh();
    }
  }

  fn policy_edit_blocker(&self) -> Option<PolicyBlocker> {
    self.maybe_admin.as_ref()?;
    if self.role != Role::Owner {
      return Some(PolicyBlocker::NotOwner);
    }
    (!self.chat.can_sign_workspace()).then_some(PolicyBlocker::NoSigningKeys)
  }

  fn admin_owner_roster(&self) -> Option<teamcraft_client_api::admin::OwnerRosterStatus> {
    self.maybe_admin.as_ref()?;
    self.chat.owner_roster()
  }

  fn admin_role_holders(&self) -> (Vec<UserId>, Vec<UserId>) {
    match &self.maybe_admin {
      Some(admin) => admin.roster_candidates(),
      None => (Vec::new(), Vec::new()),
    }
  }

  fn admin_genesis_roster(&self) -> Option<Vec<teamcraft_client_api::admin::RosterEntry>> {
    let admin = self.maybe_admin.as_ref()?;
    if self.policy_edit_blocker().is_some() || self.chat.owner_roster().is_some() {
      return None;
    }
    let (owners, admins) = admin.roster_candidates();
    Some(teamcraft_client_api::admin::genesis_roster(
      self.chat.me().id,
      &owners,
      &admins,
    ))
  }

  fn admin_identity_keys(&self) -> teamcraft_client_api::remote::SecuritySnapshot {
    self.chat.security_keys()
  }

  fn admin_publish_roster(&self, base_version: u64, members: Vec<teamcraft_client_api::admin::RosterEntry>) {
    // The form is read-only otherwise; the core refuses a non-owner as well.
    if self.policy_edit_blocker().is_some() {
      return;
    }
    let Some(admin) = &self.maybe_admin else {
      return;
    };
    self.chat.publish_roster(base_version, members);
    admin.queue_refresh_after_publish();
  }

  fn admin_brands(&self) -> Option<AdminBrandsResponse> {
    self.maybe_admin.as_ref().and_then(|admin| admin.brands())
  }

  fn create_brand(&self, request: CreateBrandRequest) {
    if let Some(admin) = &self.maybe_admin {
      admin.create_brand(request);
    }
  }

  fn update_brand(&self, id: String, request: UpdateBrandRequest) {
    if let Some(admin) = &self.maybe_admin {
      admin.update_brand(&id, request);
    }
  }

  fn delete_brand(&self, id: String) {
    if let Some(admin) = &self.maybe_admin {
      admin.delete_brand(&id);
    }
  }

  fn take_over_brand(&self, id: String) {
    if let Some(admin) = &self.maybe_admin {
      admin.take_over_brand(&id);
    }
  }

  fn brand_asset_preview(&self, id: &str, slot: &BrandAssetSlot) -> Option<String> {
    self
      .maybe_admin
      .as_ref()
      .and_then(|admin| admin.brand_asset_preview(id, slot))
  }

  fn upload_brand_asset(&self, id: String, slot: BrandAssetSlot, bytes: Vec<u8>, content_type: String) {
    if let Some(admin) = &self.maybe_admin {
      admin.upload_brand_asset(&id, &slot, bytes, content_type);
    }
  }

  fn delete_brand_asset(&self, id: String, slot: BrandAssetSlot) {
    if let Some(admin) = &self.maybe_admin {
      admin.delete_brand_asset(&id, &slot);
    }
  }

  fn mail_settings(&self) -> Option<AdminMailSettingsResponse> {
    self.maybe_admin.as_ref().and_then(|admin| admin.mail_settings())
  }

  fn notetaker_settings(&self) -> Option<teamcraft_proto::AdminNotetakerResponse> {
    self
      .maybe_admin
      .as_ref()
      .and_then(|admin| admin.notetaker_settings())
  }

  fn set_notetaker_key(&self, key: teamcraft_proto::SecretUpdate) {
    if let Some(admin) = &self.maybe_admin {
      admin.set_notetaker_key(key);
    }
  }

  fn save_mail_settings(&self, request: UpdateMailSettingsRequest) {
    if let Some(admin) = &self.maybe_admin {
      admin.save_mail_settings(request);
    }
  }

  fn reset_mail_settings(&self) {
    if let Some(admin) = &self.maybe_admin {
      admin.reset_mail_settings();
    }
  }

  fn send_test_mail(&self, request: SendTestMailRequest) {
    if let Some(admin) = &self.maybe_admin {
      admin.send_test_mail(request);
    }
  }

  fn calendar_settings(&self) -> Option<AdminCalendarSettingsResponse> {
    self
      .maybe_admin
      .as_ref()
      .and_then(|admin| admin.calendar_settings())
  }

  fn save_calendar_settings(&self, request: UpdateCalendarSettingsRequest) {
    if let Some(admin) = &self.maybe_admin {
      admin.save_calendar_settings(request);
    }
  }

  fn reset_calendar_settings(&self) {
    if let Some(admin) = &self.maybe_admin {
      admin.reset_calendar_settings();
    }
  }

  fn test_calendar(&self) {
    if let Some(admin) = &self.maybe_admin {
      admin.test_calendar();
    }
  }

  fn last_settings_test(&self) -> Option<(SettingsTestKind, SettingsTestResponse)> {
    self
      .maybe_admin
      .as_ref()
      .and_then(|admin| admin.last_settings_test())
  }

  fn runtime_settings(&self) -> Option<RuntimeSettingsResponse> {
    self
      .maybe_admin
      .as_ref()
      .and_then(|admin| admin.runtime_settings())
  }

  fn save_runtime_settings(&self, section: RuntimeSettingsSection) {
    if let Some(admin) = &self.maybe_admin {
      admin.save_runtime_settings(section);
    }
  }

  fn reset_runtime_settings(&self, key: RuntimeSettingsKey) {
    if let Some(admin) = &self.maybe_admin {
      admin.reset_runtime_settings(key);
    }
  }

  fn admin_error(&self) -> Option<String> {
    self
      .maybe_admin
      .as_ref()
      .and_then(AdminLive::maybe_error)
      .or_else(|| self.chat.last_action_error())
  }
}

impl<C: ChatBackend> SecurityBackend for Hybrid<C> {
  fn my_devices(&self) -> Vec<DeviceInfo> {
    self.chat.own_devices()
  }

  fn remove_my_device(&self, device: DeviceId) {
    self.chat.revoke_own_device(device);
  }

  fn refresh_my_devices(&self) {
    self.chat.refresh_own_devices();
  }

  fn rename_my_device(&self, device: DeviceId, name: String) {
    self.chat.rename_own_device(device, name);
  }

  fn test_push_my_device(&self, device: DeviceId) {
    self.chat.test_push_own_device(device);
  }

  /// `None` while the core has not reported it (or there is no core).
  fn key_backup(&self) -> Option<teamcraft_client_api::security::KeyBackupState> {
    self.chat.security_keys().maybe_backup
  }

  fn key_change(&self, conversation: ConversationId) -> Option<KeyChange> {
    let keys = self.chat.security_keys();
    let members = self.chat.conversation(conversation)?.member_ids;
    keys.contacts.into_iter().find_map(|contact| {
      let new_fingerprint = contact.maybe_new_fingerprint?;
      members.contains(&contact.user).then_some(KeyChange {
        user: contact.user,
        conversation,
        old_fingerprint: contact.fingerprint,
        new_fingerprint,
        // The core does not record when the server first showed the new key.
        changed_at: 0,
      })
    })
  }

  fn acknowledge_key_change(&self, conversation: ConversationId) {
    if let Some(change) = self.key_change(conversation) {
      // A new key resets the verification: it was done for the old one.
      store_string(&verified_key(change.user), "");
      self.chat.accept_key_change(change.user);
    }
  }

  /// Empty while the core has not reported its keys (or there is no core).
  fn my_identity_fingerprint(&self) -> String {
    self.chat.security_keys().my_fingerprint
  }

  fn contact_verification(&self, user: UserId) -> crate::security::ContactVerification {
    let keys = self.chat.security_keys();
    // No pinned key for this person yet: nothing to compare.
    let Some(contact) = keys.contacts.iter().find(|contact| contact.user == user) else {
      return crate::security::ContactVerification::default();
    };
    crate::security::ContactVerification {
      safety_number: crate::security::safety_number(&keys.my_fingerprint, &contact.fingerprint),
      is_verified: load_string(&verified_key(user)).as_deref() == Some(contact.fingerprint.as_str()),
    }
  }

  fn set_contact_verified(&self, user: UserId, is_verified: bool) {
    let keys = self.chat.security_keys();
    // Without a pinned key there is nothing the mark could name.
    if let Some(contact) = keys.contacts.iter().find(|contact| contact.user == user) {
      // The mark names the fingerprint it was made for, so a changed key is never shown as verified.
      store_string(
        &verified_key(user),
        if is_verified { &contact.fingerprint } else { "" },
      );
    }
  }
}

fn verified_key(user: UserId) -> String {
  format!("teamcraft.verified.{user}")
}
