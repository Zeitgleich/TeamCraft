//! Composes a real chat backend with the parts that are still demo data.
//!
//! The chat (spaces, conversations, messages) comes from `C`: the client core over the desktop
//! bridge, or the REST reader of the browser build. Search, calls, administration and device security
//! have no real backend yet and are answered by the mock, so every screen stays usable. The role
//! comes from the account, so admins see the console (with demo data).

use std::rc::Rc;

use teamcraft_proto::Role;

use crate::admin::{
  AdminChannel, AdminUser, AuditEntry, DeviceInfo, Retention, SecurityPolicy, SelfJoinOutcome,
};
use crate::backend::{AdminBackend, CallBackend, ChatBackend, SearchBackend, SecurityBackend};
use crate::calls::{HuddleInfo, MediaDevices, Participant};
use crate::mock::MockBackend;
use crate::model::{
  Conversation, ConversationId, CustomStatus, DeviceId, Message, MessageId, PresenceStatus, Space, SpaceId,
  ThreadId, ThreadSummary, TimestampMs, User, UserId,
};
use crate::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};
use crate::security::KeyChange;

pub struct Hybrid<C> {
  chat: C,
  role: Role,
  mock: MockBackend,
}

impl<C: ChatBackend> Hybrid<C> {
  pub fn new(chat: C, role: Role, now: TimestampMs) -> Self {
    Self {
      chat,
      role,
      mock: MockBackend::new(now),
    }
  }
}

impl<C: ChatBackend> ChatBackend for Hybrid<C> {
  fn set_change_notifier(&self, notify: Rc<dyn Fn()>) {
    self.chat.set_change_notifier(notify);
  }

  fn me(&self) -> User {
    self.chat.me()
  }

  fn user(&self, id: UserId) -> Option<User> {
    self.chat.user(id)
  }

  fn users(&self) -> Vec<User> {
    self.chat.users()
  }

  fn spaces(&self) -> Vec<Space> {
    self.chat.spaces()
  }

  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation> {
    self.chat.space_conversations(space)
  }

  fn direct_conversations(&self) -> Vec<Conversation> {
    self.chat.direct_conversations()
  }

  fn conversation(&self, id: ConversationId) -> Option<Conversation> {
    self.chat.conversation(id)
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
    self.chat.followed_threads()
  }

  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    self.chat.post_message(conversation, maybe_thread, body);
  }

  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str) {
    self.chat.toggle_reaction(conversation, message, emoji);
  }

  fn mark_read(&self, conversation: ConversationId) {
    self.chat.mark_read(conversation);
  }

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    self.chat.set_favorite(conversation, is_favorite);
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.chat.set_presence(presence);
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    self.chat.set_custom_status(maybe_status);
  }
}

impl<C> SearchBackend for Hybrid<C> {
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

impl<C> AdminBackend for Hybrid<C> {
  fn my_role(&self) -> Role {
    self.role.clone()
  }

  fn admin_channels(&self) -> Vec<AdminChannel> {
    self.mock.admin_channels()
  }

  fn admin_users(&self) -> Vec<AdminUser> {
    self.mock.admin_users()
  }

  fn security_policy(&self) -> SecurityPolicy {
    self.mock.security_policy()
  }

  fn set_security_policy(&self, policy: SecurityPolicy) {
    self.mock.set_security_policy(policy);
  }

  fn set_channel_retention(&self, channel: ConversationId, retention: Retention) {
    self.mock.set_channel_retention(channel, retention);
  }

  fn remove_device(&self, user: UserId, device: DeviceId) {
    self.mock.remove_device(user, device);
  }

  fn set_user_active(&self, user: UserId, is_active: bool) {
    self.mock.set_user_active(user, is_active);
  }

  fn start_org_recovery(&self, user: UserId) {
    self.mock.start_org_recovery(user);
  }

  fn admin_join_channel(&self, channel: ConversationId) -> SelfJoinOutcome {
    self.mock.admin_join_channel(channel)
  }

  fn audit_log(&self) -> Vec<AuditEntry> {
    self.mock.audit_log()
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
