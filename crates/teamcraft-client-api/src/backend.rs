//! The data boundary of the UI. Components only talk to [`ChatBackend`]; the mock implementation in
//! `teamcraft-core`'s `CoreChatBackend` implements them over the encrypted local store; the UI crate
//! ships a mock for development.
//!
//! The trait is synchronous on purpose: it models the *local, already decrypted* store of the client
//! core. Network and decryption happen below it and arrive as store updates, which the UI picks up by
//! re-reading after a revision bump (`AppState::revision` in `teamcraft-ui`).

use teamcraft_proto::Role;

use crate::admin::{AdminChannel, AdminUser, AuditEntry, Retention, SecurityPolicy, SelfJoinOutcome};
use crate::calls::{
  ActiveCall, BackgroundEffect, CallRequest, HuddleInfo, IncomingCall, MediaDevices, Participant,
};
use crate::model::{
  Conversation, ConversationId, CustomStatus, DeviceId, Message, MessageId, PresenceStatus, Space, SpaceId,
  ThreadId, ThreadSummary, User, UserId,
};
use crate::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};

pub trait ChatBackend {
  /// Gives backends that load data in the background (over IPC or the network) a way to tell the UI
  /// that something changed and views should re-read. Backends whose reads are instant ignore it.
  fn set_change_notifier(&self, _notify: std::rc::Rc<dyn Fn()>) {}

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

  /// Whether the user follows the thread (followed threads feed the threads inbox).
  fn is_thread_followed(&self, _conversation: ConversationId, _thread: ThreadId) -> bool {
    true
  }
  fn set_thread_followed(&self, _conversation: ConversationId, _thread: ThreadId, _is_followed: bool) {}
  /// The user looked at the thread: its unread counters reset.
  fn mark_thread_read(&self, _conversation: ConversationId, _thread: ThreadId) {}
  /// Members who have read up to (or past) the message in a DM or group DM. Empty elsewhere and
  /// when receipts are off.
  fn seen_by(&self, _conversation: ConversationId, _message: MessageId) -> Vec<UserId> {
    Vec::new()
  }

  /// Whether this backend holds the keys to sign workspace policy and add the admin to channels
  /// (the client core on the desktop; the browser build has none).
  fn can_sign_workspace(&self) -> bool {
    false
  }
  /// Signs and publishes a policy version. When the workspace has no signed roster yet, `owners`
  /// and `admins` become the first one. Needs [`Self::can_sign_workspace`].
  fn publish_policy(
    &self,
    _policy: teamcraft_proto::WorkspacePolicy,
    _owners: Vec<UserId>,
    _admins: Vec<UserId>,
  ) {
  }
  /// Adds the signed-in admin to a conversation through the encryption group (admin.md §2.2).
  fn admin_add_self(&self, _conversation: ConversationId) {}
  /// The newest failure of an action sent to the core, in words for the user.
  fn last_action_error(&self) -> Option<String> {
    None
  }
}

/// Calls and huddles. Media runs in `teamcraft-media`; this is the roster and device view of it,
/// and what the user does in a call. Actions return at once; the result shows up in the reads
/// after a change notification. The defaults suit backends without calls.
pub trait CallBackend {
  /// Cameras, microphones and speakers the OS reports.
  fn media_devices(&self) -> MediaDevices;
  /// The other people in the call of a conversation, as seen by the media layer.
  fn call_participants(&self, conversation: ConversationId) -> Vec<Participant>;
  /// The drop-in huddle running in a channel, if any.
  fn huddle(&self, conversation: ConversationId) -> Option<HuddleInfo>;

  /// The call this device is in.
  fn active_call(&self) -> Option<ActiveCall> {
    None
  }
  /// Calls ringing for the user that they haven't answered or declined.
  fn incoming_calls(&self) -> Vec<IncomingCall> {
    Vec::new()
  }
  /// Joins (or starts) the conversation's call, leaving any other call first.
  fn join_call(&self, _request: CallRequest) {}
  fn leave_call(&self) {}
  /// Stops ringing for this call on this device; the call goes on for the others.
  fn decline_call(&self, _conversation: ConversationId) {}
  fn set_call_muted(&self, _is_muted: bool) {}
  fn set_call_deafened(&self, _is_deafened: bool) {}
  /// A [`crate::calls::MediaDevice::id`]; `None` follows the system default.
  fn select_microphone(&self, _maybe_device: Option<String>) {}
  fn select_speakers(&self, _maybe_device: Option<String>) {}
  fn set_call_camera(&self, _is_on: bool) {}
  fn select_camera(&self, _maybe_device: Option<String>) {}
  fn set_background_effect(&self, _effect: BackgroundEffect) {}
  fn start_screen_share(&self) {}
  fn stop_screen_share(&self) {}
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

  /// Called when the console opens: backends that read the server fetch fresh data now.
  fn admin_refresh(&self) {}
  /// Whether the signed policy stores the field; the console disables the controls of others.
  fn is_policy_field_stored(&self, _field: crate::admin::PolicyField) -> bool {
    true
  }
  /// Why the signed policy cannot be changed here (not an owner, no encryption keys on this device).
  fn policy_edit_blocker(&self) -> Option<crate::admin::PolicyBlocker> {
    None
  }
  /// The last administrative action that the server or the core refused, in words for the user.
  fn admin_error(&self) -> Option<String> {
    None
  }
}

/// The user's own devices and identity-key warnings.
pub trait SecurityBackend {
  fn my_devices(&self) -> Vec<crate::admin::DeviceInfo>;
  fn remove_my_device(&self, device: DeviceId);
  /// A changed identity key of someone in this conversation that the user has not acknowledged.
  fn key_change(&self, conversation: ConversationId) -> Option<crate::security::KeyChange>;
  fn acknowledge_key_change(&self, conversation: ConversationId);
  /// Fingerprint of the user's own identity key.
  fn my_identity_fingerprint(&self) -> String;
  /// Safety number and trust state of a contact.
  fn contact_verification(&self, user: UserId) -> crate::security::ContactVerification;
  fn set_contact_verified(&self, user: UserId, is_verified: bool);
}

/// Full-text search over everything the user can read. `teamcraft-core` will answer from encrypted
/// index segments on the device; the UI only sees this interface.
pub trait SearchBackend {
  /// Messages matching the query, newest first. An empty query returns nothing.
  fn search(&self, query: &ParsedQuery) -> SearchResults;
  /// Messages that mention the user, newest first.
  fn recent_mentions(&self) -> Vec<SearchHit>;
  fn saved_searches(&self) -> Vec<SavedSearch>;
  fn save_search(&self, name: &str, query: &str);
  fn delete_saved_search(&self, id: u64);
}

/// Everything the UI needs from the client core.
pub trait Backend: ChatBackend + CallBackend + AdminBackend + SecurityBackend + SearchBackend {}

impl<T: ChatBackend + CallBackend + AdminBackend + SecurityBackend + SearchBackend> Backend for T {}
