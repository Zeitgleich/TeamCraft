//! Composes a real chat backend with the parts that are still demo data.
//!
//! The chat (spaces, conversations, messages) comes from `C`: the client core over the desktop
//! bridge, or the REST reader of the browser build. Search, calls, administration and device security
//! have no real backend yet and are answered by the mock, so every screen stays usable. The role
//! comes from the account, so admins see the console (with demo data).

use std::rc::Rc;

use teamcraft_proto::Role;

use crate::admin::{
  AdminChannel, AdminUser, AuditEntry, DeviceInfo, PolicyBlocker, PolicyField, Retention, SecurityPolicy,
  SelfJoinOutcome,
};
use crate::admin_live::{AdminLive, policy_from_security};
use crate::auth::Authenticator;
use crate::backend::{AdminBackend, CallBackend, ChatBackend, SearchBackend, SecurityBackend};
use crate::calls::{HuddleInfo, MediaDevices, Participant};
use crate::clock::now_ms;
use crate::live::{LiveScope, LiveServer, POLL_INTERVAL_MS, thread_summary};
use crate::mock::MockBackend;
use crate::model::{
  Conversation, ConversationId, CustomStatus, DeviceId, Message, MessageId, PresenceStatus, Space, SpaceId,
  ThreadId, ThreadSummary, TimestampMs, User, UserId,
};
use crate::reactive::use_interval;
use crate::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};
use crate::security::KeyChange;

pub struct Hybrid<C> {
  chat: Rc<C>,
  role: Role,
  mock: MockBackend,
  maybe_live: Option<LiveServer>,
  /// The administration console on the admin API; only for admins and owners.
  maybe_admin: Option<AdminLive>,
}

impl<C: ChatBackend + 'static> Hybrid<C> {
  /// Chat only: whatever `chat` knows is all there is (demo and tests).
  pub fn new(chat: C, role: Role, now: TimestampMs) -> Self {
    Self {
      chat: Rc::new(chat),
      role,
      mock: MockBackend::new(now),
      maybe_live: None,
      maybe_admin: None,
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
    self.chat.messages(conversation, start, len)
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
  /// Search needs decrypted messages, which the server cannot offer; until `teamcraft-core` provides
  /// the local index, the mock answers so the screens can be used.
  fn search(&self, query: &ParsedQuery) -> SearchResults {
    self.mock.search(query)
  }

  fn recent_mentions(&self) -> Vec<SearchHit> {
    self.mock.recent_mentions()
  }

  fn saved_searches(&self) -> Vec<SavedSearch> {
    self.mock.saved_searches()
  }

  fn save_search(&self, name: &str, query: &str) {
    self.mock.save_search(name, query);
  }

  fn delete_saved_search(&self, id: u64) {
    self.mock.delete_saved_search(id);
  }
}

impl<C: ChatBackend> CallBackend for Hybrid<C> {
  fn media_devices(&self) -> MediaDevices {
    self.mock.media_devices()
  }

  fn call_participants(&self, conversation: ConversationId) -> Vec<Participant> {
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

  fn huddle(&self, _conversation: ConversationId) -> Option<HuddleInfo> {
    None
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

impl<C> SecurityBackend for Hybrid<C> {
  fn my_devices(&self) -> Vec<DeviceInfo> {
    self.mock.my_devices()
  }

  fn remove_my_device(&self, device: DeviceId) {
    self.mock.remove_my_device(device);
  }

  fn key_change(&self, _conversation: ConversationId) -> Option<KeyChange> {
    None
  }

  fn acknowledge_key_change(&self, _conversation: ConversationId) {}

  fn my_identity_fingerprint(&self) -> String {
    self.mock.my_identity_fingerprint()
  }

  fn contact_verification(&self, user: UserId) -> crate::security::ContactVerification {
    self.mock.contact_verification(user)
  }

  fn set_contact_verified(&self, user: UserId, is_verified: bool) {
    self.mock.set_contact_verified(user, is_verified);
  }
}
