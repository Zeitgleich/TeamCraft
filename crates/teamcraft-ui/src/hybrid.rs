//! Composes a real chat backend with the parts that are still demo data.
//!
//! The chat (spaces, conversations, messages) comes from `C`: the client core over the desktop
//! bridge, or the REST reader of the browser build. Search, calls, administration and device security
//! have no real backend yet and are answered by the mock, so every screen stays usable. The role
//! comes from the account, so admins see the console (with demo data).

use std::rc::Rc;

use teamcraft_proto::Role;

use teamcraft_client_api::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, FileInfo, MessageExtras, NewChannel, NewSpace,
  OutgoingMessage, PermalinkTarget, SavedItem, ShareLink, ShareLinkOptions, SpaceChanges, SpaceInvite,
};
use teamcraft_proto::{BackgroundId, CategoryId, HistoryPolicy};

use crate::admin::{
  AdminChannel, AdminUser, AuditEntry, DeviceInfo, PolicyBlocker, PolicyField, Retention, SecurityPolicy,
  SelfJoinOutcome,
};
use crate::admin_live::{AdminLive, policy_from_security};
use crate::auth::Authenticator;
use crate::backend::{AdminBackend, CallBackend, ChatBackend, SearchBackend, SecurityBackend};
use crate::calls::{
  ActiveCall, BackgroundEffect, BackgroundImageInfo, CallRequest, HuddleInfo, IncomingCall, MediaDevices,
  Participant,
};
use crate::clock::now_ms;
use crate::live::{LiveScope, LiveServer, POLL_INTERVAL_MS, thread_summary};
use crate::mock::MockBackend;
use crate::model::{
  Conversation, ConversationId, ConversationKind, CustomStatus, DeviceId, Message, MessageId, PresenceStatus,
  Space, SpaceId, ThreadId, ThreadSummary, TimestampMs, User, UserId, as_log_line,
};
use crate::reactive::use_interval;
use crate::remote_calls::RemoteCalls;
use crate::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};
use crate::security::KeyChange;
use crate::settings::{load_string, store_string};
use teamcraft_client_api::remote::CallAction;

pub struct Hybrid<C> {
  chat: Rc<C>,
  role: Role,
  mock: MockBackend,
  maybe_live: Option<LiveServer>,
  /// The administration console on the admin API; only for admins and owners.
  maybe_admin: Option<AdminLive>,
  /// Calls of the desktop shell; without them the call screens run on demo data.
  maybe_calls: Option<RemoteCalls>,
}

impl<C: ChatBackend + 'static> Hybrid<C> {
  /// Real calls from the desktop shell instead of the demo roster.
  #[must_use]
  pub fn with_calls(mut self, calls: RemoteCalls) -> Self {
    self.maybe_calls = Some(calls);
    self
  }

  /// Chat only: whatever `chat` knows is all there is (demo and tests).
  pub fn new(chat: C, role: Role, now: TimestampMs) -> Self {
    Self {
      chat: Rc::new(chat),
      role,
      mock: MockBackend::new(now),
      maybe_live: None,
      maybe_admin: None,
      maybe_calls: None,
    }
  }

  /// Chat plus the server's live state (unread counts, threads, presence, receipts).
  pub fn with_live(chat: C, role: Role, now: TimestampMs, auth: Rc<dyn Authenticator>) -> Self {
    let chat = Rc::new(chat);
    let scope_chat = chat.clone();
    let scope = Rc::new(move || LiveScope {
      users: scope_chat.users().into_iter().map(|user| user.id).collect(),
      direct: scope_chat
        .direct_conversations()
        .into_iter()
        .map(|conversation| conversation.id)
        .collect(),
    });
    let maybe_admin = (role >= Role::Admin).then(|| AdminLive::new(auth.clone()));
    Self {
      chat,
      role,
      mock: MockBackend::new(now),
      maybe_live: Some(LiveServer::new(auth, scope)),
      maybe_admin,
      maybe_calls: None,
    }
  }
}

impl<C: ChatBackend> Hybrid<C> {
  fn with_counts(&self, conversation: Conversation) -> Conversation {
    match &self.maybe_live {
      Some(live) => live.with_counts(conversation),
      None => conversation,
    }
  }

  /// The server knows who is online and what status they set; the chat only knows the roster.
  fn with_live_presence(&self, mut user: User) -> User {
    if let Some((presence, maybe_status)) = self
      .maybe_live
      .as_ref()
      .and_then(|live| live.presence_of(user.id))
    {
      user.presence = presence;
      user.maybe_custom_status = maybe_status;
    }
    user
  }
}

impl<C: ChatBackend> ChatBackend for Hybrid<C> {
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
    // Chat changes (sync steps) are the cue to look at the server's counters again.
    let refresher = live.clone();
    self.chat.set_change_notifier(Rc::new(move || {
      notify();
      refresher.queue_refresh();
    }));
    live.queue_refresh();
    let poller = live.clone();
    use_interval(POLL_INTERVAL_MS, move || poller.queue_refresh());
  }

  fn me(&self) -> User {
    self.with_live_presence(self.chat.me())
  }

  fn user(&self, id: UserId) -> Option<User> {
    self.chat.user(id).map(|user| self.with_live_presence(user))
  }

  fn users(&self) -> Vec<User> {
    self
      .chat
      .users()
      .into_iter()
      .map(|user| self.with_live_presence(user))
      .collect()
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

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    self.chat.set_favorite(conversation, is_favorite);
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.chat.set_presence(presence);
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    match &self.maybe_live {
      Some(live) => live.set_custom_status(self.chat.me().id, maybe_status.as_ref(), now_ms()),
      None => self.chat.set_custom_status(maybe_status),
    }
  }

  fn set_activity(&self, is_idle: bool) {
    self.chat.set_activity(is_idle);
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

  fn permalink(&self, conversation: ConversationId, message: MessageId) -> Option<String> {
    self.chat.permalink(conversation, message)
  }

  fn resolve_permalink(&self, link: &str) -> Option<PermalinkTarget> {
    self.chat.resolve_permalink(link)
  }

  fn mark_unread(&self, conversation: ConversationId, from: MessageId) {
    self.chat.mark_unread(conversation, from);
  }

  fn first_unread(&self, conversation: ConversationId) -> Option<MessageId> {
    self.chat.first_unread(conversation)
  }

  fn draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>) -> Option<String> {
    self.chat.draft(conversation, maybe_thread)
  }

  fn set_draft(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.chat.set_draft(conversation, maybe_thread, body);
  }

  fn acknowledge(&self, conversation: ConversationId, message: MessageId) {
    self.chat.acknowledge(conversation, message);
  }

  fn conversation_files(&self, conversation: ConversationId) -> Vec<FileInfo> {
    self.chat.conversation_files(conversation)
  }

  fn download_file(&self, conversation: ConversationId, message: MessageId) {
    self.chat.download_file(conversation, message);
  }

  fn create_share_link(&self, conversation: ConversationId, message: MessageId, options: ShareLinkOptions) {
    self.chat.create_share_link(conversation, message, options);
  }

  fn share_links(&self, conversation: ConversationId, message: MessageId) -> Vec<ShareLink> {
    self.chat.share_links(conversation, message)
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

  fn has_external_people(&self, conversation: ConversationId) -> bool {
    self.chat.has_external_people(conversation)
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
        .last()
        .map(|message| message.id),
    ) else {
      return;
    };
    live.mark_thread_read(conversation, thread, latest);
  }

  fn seen_by(&self, conversation: ConversationId, message: MessageId) -> Vec<UserId> {
    self
      .maybe_live
      .as_ref()
      .map_or_else(Vec::new, |live| live.seen_by(conversation, message))
  }
}

impl<C: ChatBackend> SearchBackend for Hybrid<C> {
  /// Search needs decrypted messages: the client core answers from its local index; without one the
  /// mock answers so the screens can be used.
  fn search(&self, query: &ParsedQuery) -> SearchResults {
    match self.chat.search_backend() {
      Some(real) => real.search(query),
      None => self.mock.search(query),
    }
  }

  fn recent_mentions(&self) -> Vec<SearchHit> {
    match self.chat.search_backend() {
      Some(real) => real.recent_mentions(),
      None => self.mock.recent_mentions(),
    }
  }

  fn saved_searches(&self) -> Vec<SavedSearch> {
    match self.chat.search_backend() {
      Some(real) => real.saved_searches(),
      None => self.mock.saved_searches(),
    }
  }

  fn save_search(&self, name: &str, query: &str) {
    match self.chat.search_backend() {
      Some(real) => real.save_search(name, query),
      None => self.mock.save_search(name, query),
    }
  }

  fn delete_saved_search(&self, id: u64) {
    match self.chat.search_backend() {
      Some(real) => real.delete_saved_search(id),
      None => self.mock.delete_saved_search(id),
    }
  }
}

impl<C: ChatBackend> CallBackend for Hybrid<C> {
  fn media_devices(&self) -> MediaDevices {
    match &self.maybe_calls {
      Some(calls) => calls.devices(),
      None => self.mock.media_devices(),
    }
  }

  fn call_participants(&self, conversation: ConversationId) -> Vec<Participant> {
    if let Some(calls) = &self.maybe_calls {
      let me = self.chat.me().id;
      return calls
        .participants(conversation)
        .into_iter()
        .filter(|participant| participant.user != me)
        .collect();
    }
    let members = self
      .conversation(conversation)
      .map(|conv| conv.member_ids)
      .unwrap_or_default();
    let me = self.chat.me().id;
    members
      .into_iter()
      .filter(|member| *member != me)
      .take(7)
      .map(|user| Participant {
        user,
        is_muted: false,
        is_camera_on: true,
        has_hand_raised: false,
        is_sharing_screen: false,
      })
      .collect()
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

  fn incoming_calls(&self) -> Vec<IncomingCall> {
    self
      .maybe_calls
      .as_ref()
      .map_or_else(Vec::new, RemoteCalls::incoming)
  }

  fn join_call(&self, request: CallRequest) {
    if let Some(calls) = &self.maybe_calls {
      calls.join(request);
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

  fn set_background_effect(&self, effect: BackgroundEffect) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::SetBackgroundEffect { effect });
    }
  }

  fn start_screen_share(&self) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::StartScreenShare);
    }
  }

  fn stop_screen_share(&self) {
    if let Some(calls) = &self.maybe_calls {
      calls.act(CallAction::StopScreenShare);
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
      None => self.mock.admin_channels(),
    }
  }

  fn admin_users(&self) -> Vec<AdminUser> {
    match &self.maybe_admin {
      Some(admin) => admin.users(),
      None => self.mock.admin_users(),
    }
  }

  fn security_policy(&self) -> SecurityPolicy {
    match &self.maybe_admin {
      Some(admin) => admin.security_policy(),
      None => self.mock.security_policy(),
    }
  }

  fn set_security_policy(&self, policy: SecurityPolicy) {
    let Some(admin) = &self.maybe_admin else {
      self.mock.set_security_policy(policy);
      return;
    };
    let merged = policy_from_security(&admin.policy(), &policy);
    let (owners, admins) = admin.roster_candidates();
    self.chat.publish_policy(merged, owners, admins);
    admin.queue_refresh_after_publish();
  }

  fn set_channel_retention(&self, channel: ConversationId, retention: Retention) {
    match &self.maybe_admin {
      Some(admin) => admin.set_channel_retention(channel, retention),
      None => self.mock.set_channel_retention(channel, retention),
    }
  }

  fn remove_device(&self, user: UserId, device: DeviceId) {
    match &self.maybe_admin {
      Some(admin) => admin.remove_device(user, device),
      None => self.mock.remove_device(user, device),
    }
  }

  fn set_user_active(&self, user: UserId, is_active: bool) {
    match &self.maybe_admin {
      Some(admin) => admin.set_user_active(user, is_active),
      None => self.mock.set_user_active(user, is_active),
    }
  }

  fn start_org_recovery(&self, user: UserId) {
    // Organization recovery needs the recovery key ceremony of the core; not wired into the console yet.
    if self.maybe_admin.is_none() {
      self.mock.start_org_recovery(user);
    }
  }

  fn admin_join_channel(&self, channel: ConversationId) -> SelfJoinOutcome {
    let Some(admin) = &self.maybe_admin else {
      return self.mock.admin_join_channel(channel);
    };
    match admin.policy().admin_self_join {
      teamcraft_proto::AdminSelfJoin::Visible => {
        self.chat.admin_add_self(channel);
        SelfJoinOutcome::Joined
      },
      teamcraft_proto::AdminSelfJoin::OwnerApproval => SelfJoinOutcome::ApprovalRequested,
      teamcraft_proto::AdminSelfJoin::Disabled | teamcraft_proto::AdminSelfJoin::Unknown(_) => {
        SelfJoinOutcome::Disabled
      },
    }
  }

  fn audit_log(&self) -> Vec<AuditEntry> {
    match &self.maybe_admin {
      Some(admin) => admin.audit(),
      None => self.mock.audit_log(),
    }
  }

  fn admin_refresh(&self) {
    if let Some(admin) = &self.maybe_admin {
      admin.queue_refresh();
    }
  }

  fn is_policy_field_stored(&self, field: PolicyField) -> bool {
    self.maybe_admin.is_none()
      || matches!(
        field,
        PolicyField::AdminSelfJoin | PolicyField::OrgRecovery | PolicyField::DefaultHistory
      )
  }

  fn policy_edit_blocker(&self) -> Option<PolicyBlocker> {
    self.maybe_admin.as_ref()?;
    if self.role != Role::Owner {
      return Some(PolicyBlocker::NotOwner);
    }
    (!self.chat.can_sign_workspace()).then_some(PolicyBlocker::NoSigningKeys)
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
    self.mock.my_devices()
  }

  fn remove_my_device(&self, device: DeviceId) {
    self.mock.remove_my_device(device);
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

  fn my_identity_fingerprint(&self) -> String {
    let keys = self.chat.security_keys();
    if keys.my_fingerprint.is_empty() {
      self.mock.my_identity_fingerprint()
    } else {
      keys.my_fingerprint
    }
  }

  fn contact_verification(&self, user: UserId) -> crate::security::ContactVerification {
    let keys = self.chat.security_keys();
    let Some(contact) = keys.contacts.iter().find(|contact| contact.user == user) else {
      return self.mock.contact_verification(user);
    };
    crate::security::ContactVerification {
      safety_number: crate::security::safety_number(&keys.my_fingerprint, &contact.fingerprint),
      is_verified: load_string(&verified_key(user)).as_deref() == Some(contact.fingerprint.as_str()),
    }
  }

  fn set_contact_verified(&self, user: UserId, is_verified: bool) {
    let keys = self.chat.security_keys();
    match keys.contacts.iter().find(|contact| contact.user == user) {
      // The mark names the fingerprint it was made for, so a changed key is never shown as verified.
      Some(contact) => store_string(
        &verified_key(user),
        if is_verified { &contact.fingerprint } else { "" },
      ),
      None => self.mock.set_contact_verified(user, is_verified),
    }
  }
}

fn verified_key(user: UserId) -> String {
  format!("teamcraft.verified.{user}")
}
