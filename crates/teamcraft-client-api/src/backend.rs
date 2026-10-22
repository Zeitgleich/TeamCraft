//! The data boundary of the UI. Components only talk to [`ChatBackend`]; the mock implementation in
//! `teamcraft-core`'s `CoreChatBackend` implements them over the encrypted local store; the UI crate
//! ships a mock for development.
//!
//! The trait is synchronous on purpose: it models the *local, already decrypted* store of the client
//! core. Network and decryption happen below it and arrive as store updates, which the UI picks up by
//! re-reading after a revision bump (`AppState::revision` in `teamcraft-ui`).

use teamcraft_proto::{CategoryId, HistoryPolicy, Role};

use crate::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, FileInfo, MessageExtras, NewChannel, NewSpace,
  OutgoingMessage, PermalinkTarget, SavedItem, ShareLink, ShareLinkOptions, SpaceChanges, SpaceInvite,
};
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
  /// Identity keys this device knows (empty when the backend has no encryption keys).
  fn security_keys(&self) -> crate::remote::SecuritySnapshot {
    crate::remote::SecuritySnapshot::default()
  }
  /// Trusts the contact's new identity key after the user compared it.
  fn accept_key_change(&self, _user: UserId) {}
  /// The device became idle or active; the server derives "away" from it.
  fn set_activity(&self, _is_idle: bool) {}
  /// Whether the person is in a call right now (server roster), shown next to their name.
  fn is_in_call(&self, _user: UserId) -> bool {
    false
  }
  /// The backend's own search, saved searches and mentions, when it has them (the client core); the
  /// UI answers those screens with demo data otherwise.
  fn search_backend(&self) -> Option<&dyn SearchBackend> {
    None
  }
  /// The newest failure of an action sent to the core, in words for the user.
  fn last_action_error(&self) -> Option<String> {
    None
  }

  // Message actions (crate::actions). Defaults suit backends without them.

  /// Sends a message with everything the composer can set. The default sends the plain text.
  fn send_message(&self, message: OutgoingMessage) {
    self.post_message(message.conversation, message.maybe_thread, &message.body);
  }
  /// Replies, pins, files, previews, edits and the rest for messages on screen.
  fn message_extras(&self, _conversation: ConversationId, _messages: &[MessageId]) -> Vec<MessageExtras> {
    Vec::new()
  }
  /// Replaces the text of one of the user's own messages; earlier versions stay visible as history.
  fn edit_message(&self, _conversation: ConversationId, _message: MessageId, _body: &str) {}
  /// Deletes one of the user's own messages for everyone.
  fn delete_message(&self, _conversation: ConversationId, _message: MessageId) {}
  /// Sends a copy of a message (re-encrypted for the target) with an optional comment.
  fn forward_message(&self, _from: ConversationId, _message: MessageId, _to: ConversationId, _comment: &str) {
  }
  fn set_pinned(&self, _conversation: ConversationId, _message: MessageId, _is_pinned: bool) {}
  fn pinned_messages(&self, _conversation: ConversationId) -> Vec<Message> {
    Vec::new()
  }
  /// Saves a message for later (personal, synced to the user's devices, encrypted).
  fn set_saved(&self, _conversation: ConversationId, _message: MessageId, _is_saved: bool) {}
  fn saved_items(&self) -> Vec<SavedItem> {
    Vec::new()
  }
  /// A link to the message that only members of the conversation can open.
  fn permalink(&self, _conversation: ConversationId, _message: MessageId) -> Option<String> {
    None
  }
  /// Where a permalink points, if the user is a member there.
  fn resolve_permalink(&self, _link: &str) -> Option<PermalinkTarget> {
    None
  }
  /// Marks the conversation unread from this message on.
  fn mark_unread(&self, _conversation: ConversationId, _from: MessageId) {}
  /// The first message the user hasn't read, to jump to.
  fn first_unread(&self, _conversation: ConversationId) -> Option<MessageId> {
    None
  }
  /// The unsent text of a conversation or thread (synced to the user's devices, encrypted).
  fn draft(&self, _conversation: ConversationId, _maybe_thread: Option<ThreadId>) -> Option<String> {
    None
  }
  /// Stores the draft; an empty body removes it. Call debounced while typing.
  fn set_draft(&self, _conversation: ConversationId, _maybe_thread: Option<ThreadId>, _body: &str) {}
  /// Confirms having read a message that asked for acknowledgement.
  fn acknowledge(&self, _conversation: ConversationId, _message: MessageId) {}

  // Files.

  /// Files of a conversation, newest first.
  fn conversation_files(&self, _conversation: ConversationId) -> Vec<FileInfo> {
    Vec::new()
  }
  /// Fetches and decrypts a file; its [`FileInfo::transfer`] reports progress and the local path.
  fn download_file(&self, _conversation: ConversationId, _message: MessageId) {}
  /// Creates a public download link for a file (e2ee.md §4.5.1); it appears in [`Self::share_links`].
  fn create_share_link(
    &self,
    _conversation: ConversationId,
    _message: MessageId,
    _options: ShareLinkOptions,
  ) {
  }
  fn share_links(&self, _conversation: ConversationId, _message: MessageId) -> Vec<ShareLink> {
    Vec::new()
  }
  /// Largest file the server accepts, if known.
  fn max_upload_bytes(&self) -> Option<u64> {
    None
  }

  // Spaces and channels.

  fn create_space(&self, _space: NewSpace) {}
  fn update_space(&self, _space: SpaceId, _changes: SpaceChanges) {}
  /// Founds a channel with the options of the "new channel" dialog.
  fn create_channel_with(&self, _channel: NewChannel) {}
  fn update_channel(&self, _conversation: ConversationId, _changes: ChannelChanges) {}
  /// Public channels of a space, joined or not (the channel browser).
  fn browse_channels(&self, _space: SpaceId) -> Vec<BrowsableChannel> {
    Vec::new()
  }
  fn join_channel(&self, _conversation: ConversationId) {}
  fn leave_channel(&self, _conversation: ConversationId) {}
  fn categories(&self, _space: SpaceId) -> Vec<CategoryInfo> {
    Vec::new()
  }
  fn create_category(&self, _space: SpaceId, _name: &str) {}
  fn rename_category(&self, _space: SpaceId, _category: CategoryId, _name: &str) {}
  fn delete_category(&self, _space: SpaceId, _category: CategoryId) {}
  /// Adds a person with an explicit history choice ("share history" toggle, e2ee.md §4.3).
  fn add_member_with_history(&self, _conversation: ConversationId, _user: UserId, _history: HistoryPolicy) {}
  /// Invites someone by email to a space, as member or guest.
  fn invite_to_space(&self, _invite: SpaceInvite) {}
  /// "Jump to date": asks for the history around `at` (Unix milliseconds) to be loaded. It arrives
  /// like any store update; [`Self::is_history_loaded`] tells when the request is answered.
  fn request_history_at(&self, _conversation: ConversationId, _at: i64) {}
  /// Whether the request for `at` was answered (also when it failed or found nothing). Backends
  /// that hold the whole history always say yes.
  fn is_history_loaded(&self, _conversation: ConversationId, _at: i64) -> bool {
    true
  }
  /// The conversation includes people from outside the organization (the "external" badge).
  fn has_external_people(&self, _conversation: ConversationId) -> bool {
    false
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
  fn set_push_to_talk(&self, _is_enabled: bool, _maybe_shortcut: Option<String>) {}
  /// The push-to-talk key inside the window went down (`true`) or up.
  fn set_talking(&self, _is_talking: bool) {}
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
