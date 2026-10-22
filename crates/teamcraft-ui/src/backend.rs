//! The data boundary of the UI. Components only talk to [`ChatBackend`]; the mock implementation in
//! [`crate::mock`] is replaced by an adapter over `teamcraft-core` once the client core exists.
//!
//! The trait is synchronous on purpose: it models the *local, already decrypted* store of the client
//! core. Network and decryption happen below it and arrive as store updates, which the UI picks up by
//! re-reading after a revision bump (see [`crate::state::AppState::revision`]).

use teamcraft_proto::Role;

use crate::admin::{AdminChannel, AdminUser, AuditEntry, Retention, SecurityPolicy, SelfJoinOutcome};
use crate::calls::{HuddleInfo, MediaDevices, Participant};
use crate::model::{
  Conversation, ConversationId, CustomStatus, DeviceId, Message, MessageId, PresenceStatus, Space, SpaceId,
  ThreadId, ThreadSummary, User, UserId,
};

pub trait ChatBackend {
  /// The signed-in user, including the presence and custom status they chose.
  fn me(&self) -> User;
  fn user(&self, id: UserId) -> Option<User>;
  fn users(&self) -> Vec<User>;

  fn spaces(&self) -> Vec<Space>;
  /// Channels of a space the user is a member of.
  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation>;
  /// DMs and group DMs; they live outside spaces.
  fn direct_conversations(&self) -> Vec<Conversation>;
  fn conversation(&self, id: ConversationId) -> Option<Conversation>;

  /// Number of top-level messages (thread replies excluded).
  fn message_count(&self, conversation: ConversationId) -> usize;
  /// Top-level messages `start..start + len`, oldest first. Out-of-range parts are clipped.
  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message>;
  /// The thread root followed by its replies, oldest first.
  fn thread(&self, conversation: ConversationId, thread: ThreadId) -> Vec<Message>;
  fn followed_threads(&self) -> Vec<ThreadSummary>;

  /// Posts a message, as a thread reply when `maybe_thread` is set.
  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str);
  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str);
  fn mark_read(&self, conversation: ConversationId);
  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool);

  fn set_presence(&self, presence: PresenceStatus);
  fn set_custom_status(&self, maybe_status: Option<CustomStatus>);
}

/// Calls and huddles. Media runs in `teamcraft-media`; this is the roster and device view of it.
pub trait CallBackend {
  /// Cameras, microphones and speakers the OS reports.
  fn media_devices(&self) -> MediaDevices;
  /// The other people in the call of a conversation, as seen by the media layer.
  fn call_participants(&self, conversation: ConversationId) -> Vec<Participant>;
  /// The drop-in huddle running in a channel, if any.
  fn huddle(&self, conversation: ConversationId) -> Option<HuddleInfo>;
}

/// Administration without content access: every read returns metadata only.
pub trait AdminBackend {
  fn my_role(&self) -> Role;
  fn admin_channels(&self) -> Vec<AdminChannel>;
  fn admin_users(&self) -> Vec<AdminUser>;
  fn security_policy(&self) -> SecurityPolicy;
  /// Stores a new signed policy version.
  fn set_security_policy(&self, policy: SecurityPolicy);
  fn set_channel_retention(&self, channel: ConversationId, retention: Retention);
  fn remove_device(&self, user: UserId, device: DeviceId);
  fn set_user_active(&self, user: UserId, is_active: bool);
  fn start_org_recovery(&self, user: UserId);
  /// Adds the admin to a private channel according to the `admin_self_join` policy.
  fn admin_join_channel(&self, channel: ConversationId) -> SelfJoinOutcome;
  fn audit_log(&self) -> Vec<AuditEntry>;
}

/// The user's own devices and identity-key warnings.
pub trait SecurityBackend {
  fn my_devices(&self) -> Vec<crate::admin::DeviceInfo>;
  fn remove_my_device(&self, device: DeviceId);
  /// A changed identity key of someone in this conversation that the user has not acknowledged.
  fn key_change(&self, conversation: ConversationId) -> Option<crate::security::KeyChange>;
  fn acknowledge_key_change(&self, conversation: ConversationId);
}

/// Everything the UI needs from the client core.
pub trait Backend: ChatBackend + CallBackend + AdminBackend + SecurityBackend {}

impl<T: ChatBackend + CallBackend + AdminBackend + SecurityBackend> Backend for T {}
