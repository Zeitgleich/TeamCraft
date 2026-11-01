//! The data boundary of the UI. Components only talk to [`ChatBackend`];
//! `teamcraft-core`'s `CoreChatBackend` implements them over the encrypted local store; the UI crate
//! has a mock for development (feature `demo`, never in a shipped build).
//!
//! The trait is synchronous on purpose: it models the *local, already decrypted* store of the client
//! core. Network and decryption happen below it and arrive as store updates, which the UI picks up by
//! re-reading after a revision bump (`AppState::revision` in `teamcraft-ui`).

use teamcraft_proto::{
  AdminBrandsResponse, AdminCalendarSettingsResponse, AdminInstanceResponse, AdminMailSettingsResponse,
  AnnouncementResponse, BackgroundId, BrandAssetSlot, CategoryId, ChannelModeration, CreateBrandRequest,
  HistoryPolicy, OrgBackgroundId, PermissionSchemeResponse, Role, RuntimeSettingsKey,
  RuntimeSettingsResponse, RuntimeSettingsSection, SchemeChange, SendTestMailRequest, SetAnnouncementRequest,
  SettingsTestResponse, UpdateBrandRequest, UpdateCalendarSettingsRequest, UpdateMailSettingsRequest,
  WorkspacePolicy,
};

use crate::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, FileInfo, LinkedMessage, MediaAnchor, MediaPage,
  MessageExtras, NewChannel, NewSpace, OutgoingMessage, PermalinkTarget, SavedItem, ShareLink,
  ShareLinkOptions, SpaceChanges, SpaceInvite,
};
use crate::activity::ActivityFeed;
use crate::admin::{
  AdminChannel, AdminLoad, AdminSpace, AdminSpaceChanges, AdminUser, AuditEntry, AuditFilter, Retention,
  SelfJoinOutcome, SettingsTestKind,
};
use crate::calls::{
  ActiveCall, BackgroundEffect, BackgroundImageInfo, CallEndReason, CallHistoryItem, CallMoveOffer,
  CallRequest, CallStatusSounds, CallsUnavailableReason, HuddleInfo, IncomingCall, MediaDevices, Participant,
  RemoteVideoTrack, ScreenContentMode, ScreenShareResolution,
};
use crate::calls::{
  CallPreview, CameraFallback, JoinMedia, OrgBackgroundRule, OrgBackgrounds, PreviewCamera,
};
use crate::model::{
  Conversation, ConversationId, CustomStatus, DeviceId, MembershipIssue, Message, MessageId, PresenceStatus,
  Space, SpaceId, ThreadId, ThreadSummary, TimestampMs, User, UserId,
};
use crate::privacy::{LastSeenVisibility, PrivacySettings};
use crate::search::{ParsedQuery, SavedSearch, SearchHit, SearchResults};
use crate::video_stats::VideoStatsJson;

/// A list that depends on a server round trip, so it can be "not loaded yet" as well as empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadingList {
  CallHistory,
}

pub trait ChatBackend {
  /// Gives backends that load data in the background (over IPC or the network) a way to tell the UI
  /// that something changed and views should re-read. Backends whose reads are instant ignore it.
  fn set_change_notifier(&self, _notify: std::rc::Rc<dyn Fn()>) {}

  /// The announcement banner to show above the app, unless this user dismissed it.
  fn current_announcement(&self) -> Option<AnnouncementResponse> {
    None
  }

  fn dismiss_announcement(&self, _id: teamcraft_proto::AnnouncementId) {}

  /// The signed-in user, including the presence and custom status they chose.
  fn me(&self) -> User;
  /// Anyone known, deactivated accounts included (the authors of old messages).
  fn user(&self, id: UserId) -> Option<User>;
  /// The people to offer in lists, pickers and mentions: deactivated accounts are left out
  /// (issue #654).
  fn users(&self) -> Vec<User>;
  /// People whose name or username contains `query` (any case): the people the backend knows and,
  /// for a backend that reads the server, the server's directory, so colleagues who share nothing
  /// with the user yet are found too. Such a backend asks the server at most once per query in a
  /// while and answers with what it knows until the answer arrives (a change notification). The
  /// UI debounces typing before it asks. An empty query lists the first people alphabetically.
  fn search_people(&self, query: &str) -> Vec<User> {
    filter_people(self.users(), query)
  }

  fn spaces(&self) -> Vec<Space>;
  /// Channels of a space the user is a member of.
  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation>;
  /// DMs and group DMs; they live outside spaces.
  fn direct_conversations(&self) -> Vec<Conversation>;
  fn conversation(&self, id: ConversationId) -> Option<Conversation>;

  /// Number of rows of the feed, as [`Self::messages`] pages them: top-level messages and replies
  /// also sent to the conversation.
  fn message_count(&self, conversation: ConversationId) -> usize;

  /// Messages of the conversation received but not readable on this device yet (their keys have
  /// not arrived). Backends without end-to-end encryption have none.
  fn undecrypted_count(&self, _conversation: ConversationId) -> usize {
    0
  }
  /// When the newest top-level message was sent, for ordering conversations by recency.
  fn last_activity(&self, _conversation: ConversationId) -> Option<TimestampMs> {
    None
  }
  /// Feed rows `start..start + len`, oldest first, counted as [`Self::message_count`] counts
  /// them. Out-of-range parts are clipped.
  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message>;
  /// Messages `start..start + len` will be read soon (the rows just beyond what the feed shows):
  /// a backend that loads them asynchronously starts now, so they are there when scrolled to
  /// (issue #1050). Like a read, it says the rows are wanted: asked again after every change, so
  /// a backend may drop what neither was asked for. Backends that hold every message have
  /// nothing to do.
  fn prefetch_messages(&self, _conversation: ConversationId, _start: usize, _len: usize) {}
  /// [`Self::message_count`] and [`Self::messages`] together, for a feed window: a backend that
  /// reads the whole conversation for each of them reads it once here.
  fn message_page(&self, conversation: ConversationId, start: usize, len: usize) -> (usize, Vec<Message>) {
    (
      self.message_count(conversation),
      self.messages(conversation, start, len),
    )
  }
  /// The thread root followed by its replies, oldest first.
  fn thread(&self, conversation: ConversationId, thread: ThreadId) -> Vec<Message>;
  fn followed_threads(&self) -> Vec<ThreadSummary>;

  /// Posts a message, as a thread reply when `maybe_thread` is set.
  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str);
  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str);
  fn mark_read(&self, conversation: ConversationId);
  /// The user looked at `message` (an opened activity entry): the conversation's read position
  /// moves up to it, not to the end. Never moves back.
  fn mark_read_through(&self, _conversation: ConversationId, _message: MessageId) {}
  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool);

  fn set_presence(&self, presence: PresenceStatus);
  /// Sets the presence with an end time, for a quick do-not-disturb ("for 1 hour"). Backends without
  /// timed presence keep it until changed.
  fn set_presence_until(&self, presence: PresenceStatus, _maybe_until: Option<TimestampMs>) {
    self.set_presence(presence);
  }
  fn set_custom_status(&self, maybe_status: Option<CustomStatus>);
  /// Which statuses the workspace allows (PLAN.md §4): whether there are any, whether people may
  /// write their own, and the presets. The server enforces it; the UI only offers what it allows.
  fn presence_policy(&self) -> teamcraft_proto::PresencePolicy {
    teamcraft_proto::PresencePolicy::default()
  }
  /// How large screen shares may be (#565): the share options offer 4K only where the workspace
  /// allows it, and a share asked for larger is sent at the allowed size.
  fn screen_share_policy(&self) -> teamcraft_proto::ScreenSharePolicy {
    teamcraft_proto::ScreenSharePolicy::default()
  }

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
  /// The conversation is shown: read its members' receipts again, since those sent while this
  /// device was offline never arrive as events. Backends whose UI reads receipts itself ignore it.
  fn refresh_read_receipts(&self, _conversation: ConversationId) {}
  /// Unread counts, threads, presence and receipts the backend keeps current from gateway events.
  /// `None` when it has no gateway (the UI then asks the server itself).
  fn live_snapshot(&self) -> Option<crate::live::LiveSnapshot> {
    None
  }
  /// Counts the changes to the user's meetings the backend heard of (and reconnects, which may
  /// have lost some); a change means the meetings are to be read again. 0 without a gateway.
  fn meetings_revision(&self) -> u64 {
    0
  }
  /// Moves when the open device link requests changed (e2ee.md §5); the UI then reads them. 0
  /// without a core.
  fn link_request_mark(&self) -> u64 {
    0
  }
  /// Other people typing in a conversation itself right now, not in its threads (the indicator
  /// lasts a few seconds).
  fn typing_users(&self, _conversation: ConversationId) -> Vec<UserId> {
    Vec::new()
  }
  /// Everywhere other people type right now, in every conversation and thread (#882): the chat
  /// list, the headers and the typing lines all read this one answer.
  fn typing_now(&self) -> Vec<crate::model::TypingNow> {
    Vec::new()
  }
  /// The user types in a conversation, or with `maybe_thread` a reply in that thread: tell the
  /// other members (the core throttles).
  fn notify_typing(&self, _conversation: ConversationId, _maybe_thread: Option<crate::model::ThreadId>) {}

  /// Whether this backend holds the keys to sign workspace policy and add the admin to channels
  /// (the client core on the desktop; the browser build has none).
  fn can_sign_workspace(&self) -> bool {
    false
  }
  /// Signs and publishes a policy version. When the workspace has no signed roster yet, `owners`
  /// and `admins` become the first one. Needs [`Self::can_sign_workspace`].
  fn publish_policy(
    &self,
    _base: teamcraft_proto::WorkspacePolicy,
    _policy: teamcraft_proto::WorkspacePolicy,
    _owners: Vec<UserId>,
    _admins: Vec<UserId>,
  ) {
  }
  /// Signs and publishes this roster as the next version (owners only, admin.md §2.3). Needs
  /// [`Self::can_sign_workspace`].
  fn publish_roster(&self, _base_version: u64, _members: Vec<crate::admin::RosterEntry>) {}
  /// Adds the signed-in admin to a conversation through the encryption group (admin.md §2.2).
  fn admin_add_self(&self, _conversation: ConversationId) {}
  /// The signed-in admin asks the owners of a private channel to let them in (admin.md §2.4,
  /// `admin_self_join = owner_approval`).
  fn request_admin_join(&self, _conversation: ConversationId) {}
  /// Takes the signed-in admin's request back.
  fn withdraw_admin_join(&self, _conversation: ConversationId) {}
  /// A channel owner lets the admin in: the server membership, this device's MLS add and the
  /// visible "joined as a workspace admin" message. `history` is what the owner chose to share:
  /// `from_join` (what a self-join gets) unless they explicitly chose `full`.
  fn approve_admin_join(&self, _conversation: ConversationId, _user: UserId, _history: HistoryPolicy) {}
  fn decline_admin_join(&self, _conversation: ConversationId, _user: UserId) {}
  /// Removes a deactivated user from all chats with signed removals (admin.md §3.2.1). Needs
  /// [`Self::can_sign_workspace`].
  fn admin_remove_from_all_chats(&self, _user: UserId) {}
  /// Deletes an account (admin.md §3.2.1): the signed removals of
  /// [`Self::admin_remove_from_all_chats`], then the server's deletion. Needs
  /// [`Self::can_sign_workspace`].
  fn admin_delete_user(&self, _user: UserId) {}
  /// Removes a leftover of [`Self::group_leftovers`] from the encryption group with a removal this
  /// device signs and commits (issue #734). Needs [`Self::can_sign_workspace`].
  fn admin_remove_group_leftover(&self, _conversation: ConversationId, _user: UserId) {}
  /// What desktop and browser notifications may show (the verified workspace policy). A backend
  /// that does not know the policy answers with the strictest mode.
  fn push_content_mode(&self) -> teamcraft_proto::PushContentMode {
    teamcraft_proto::PushContentMode::Generic
  }
  /// The owners of the roster this device verified (admin.md §2.3); `None` when the backend has
  /// verified none.
  fn owner_roster(&self) -> Option<crate::admin::OwnerRosterStatus> {
    None
  }
  /// Identity keys this device knows (empty when the backend has no encryption keys).
  fn security_keys(&self) -> crate::remote::SecuritySnapshot {
    crate::remote::SecuritySnapshot::default()
  }
  /// Trusts the contact's new identity key after the user compared it.
  fn accept_key_change(&self, _user: UserId) {}
  /// The user opened this conversation: a catch-up still running takes it next (issue #247). Only a
  /// hint; a conversation with nothing stored yet asks the core for no messages, so this is the
  /// only way the core learns it.
  fn focus_conversation(&self, _conversation: ConversationId) {}
  /// The device became idle or active; the server derives "away" from it.
  fn set_activity(&self, _is_idle: bool) {}
  /// Whether the user is looking at this desktop or web session right now (notifications.md,
  /// "Attention"); the server holds phone pushes back while a session is attentive.
  fn set_attention(&self, _is_attentive: bool) {}
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
  /// The user's own call background pictures.
  fn background_images(&self) -> Vec<BackgroundImageInfo> {
    Vec::new()
  }
  /// Adds a picture (the host normalizes it to a JPEG without metadata first); it is encrypted,
  /// uploaded and synced to the user's devices.
  fn add_background_image(&self, _name: &str, _picture: Vec<u8>) {}
  fn delete_background_image(&self, _id: BackgroundId) {}
  /// The organization's call backgrounds and their rule, from the verified workspace policy.
  fn org_backgrounds(&self) -> OrgBackgrounds {
    OrgBackgrounds::default()
  }
  /// Owners: uploads a picture (the host normalizes it to a JPEG first) and adds it to the signed
  /// policy.
  fn add_org_background(&self, _name: &str, _picture: Vec<u8>) {}
  /// Owners: removes a picture from the signed policy and deletes it.
  fn delete_org_background(&self, _id: OrgBackgroundId) {}
  /// Owners: the rule, the default and the order of the organization's backgrounds.
  fn set_org_background_rule(&self, _rule: OrgBackgroundRule) {}
  /// A link to the message that only members of the conversation can open.
  fn permalink(&self, _conversation: ConversationId, _message: MessageId) -> Option<String> {
    None
  }
  /// Where a permalink points, if the user is a member there.
  fn resolve_permalink(&self, _link: &str) -> Option<PermalinkTarget> {
    None
  }
  /// The message a permalink points to, for its embedded preview, read from this device's store
  /// and only if the user is a member of its conversation. `None` while a backend that loads in the
  /// background is still looking, and from backends that can't read messages.
  fn linked_message(&self, _link: &str) -> Option<LinkedMessage> {
    None
  }
  /// Marks the conversation unread from this message on.
  fn mark_unread(&self, _conversation: ConversationId, _from: MessageId) {}
  /// The first message the user hasn't read, to jump to.
  /// This device is not in the conversation's encryption group yet and waits for another device of
  /// the user or a colleague to let it in; history the key bag holds is readable meanwhile.
  fn is_waiting_for_device(&self, _conversation: ConversationId) -> bool {
    false
  }
  /// Removals and leaves on their way or failed, people left in the encryption group (for admins)
  /// and a server refusing the group, for the conversation's banners.
  fn membership_issues(&self, _conversation: ConversationId) -> Vec<MembershipIssue> {
    Vec::new()
  }
  /// Tries a failed removal or leave again, or signs the removal of somebody left in the group.
  fn retry_removal(&self, _conversation: ConversationId, _user: UserId) {}
  /// "Keep": drops an unconfirmed (or failed) removal without doing anything more.
  fn discard_removal(&self, _conversation: ConversationId, _user: UserId) {}
  /// "Retry" on a DM whose encryption setup waits for a welcome or failed.
  fn retry_dm_setup(&self, _conversation: ConversationId) {}
  /// Sets up the encryption of a meeting chat this user just scheduled or started, with the users
  /// they invited (the organizer's own request: the group is never founded from the server's
  /// member list).
  fn set_up_meeting_chat(&self, _conversation: ConversationId, _invitees: Vec<UserId>) {}
  /// The organizer saved a changed invitee list of a meeting: `invitees` is the whole list, `removed`
  /// the users taken off it. The new ones go into the chat's encryption group and the removed ones
  /// out of it, signed by this device.
  fn update_meeting_chat_members(
    &self,
    _conversation: ConversationId,
    _invitees: Vec<UserId>,
    _removed: Vec<UserId>,
  ) {
  }
  /// "Add people" in a DM or group DM call (docs/meetings.md, "A call becomes a meeting"): a meeting
  /// room with the people in the call (`in_call`, as the call shows them) and `added`, whose
  /// encryption group gets exactly them, and the call moves there. The same `key` again (a retry)
  /// gets the same room.
  fn add_people_to_call(
    &self,
    _from: ConversationId,
    _in_call: Vec<UserId>,
    _added: Vec<UserId>,
    _maybe_title: Option<String>,
    _key: teamcraft_proto::IdempotencyKey,
  ) {
  }
  /// The announced move of the call of `from` into `to`, if a member of `from`'s encryption group
  /// announced one; the server's `call_move` alone shows nothing.
  fn call_move_offer(&self, _from: ConversationId, _to: ConversationId) -> Option<CallMoveOffer> {
    None
  }
  /// Checks a move before this device follows it: announced in `from`'s group, and `to`'s group
  /// holds nobody but `from`'s members and the people the announcement named. Fails otherwise.
  fn accept_call_move(&self, _from: ConversationId, _to: ConversationId) {}
  /// A host admitted `user` from a meeting's lobby: lets them into the chat's encryption, on this
  /// host's own decision (never because the server made them a member).
  fn let_into_meeting_chat(&self, _conversation: ConversationId, _user: UserId) {}
  /// The app is about to hand out the link of `meeting`: makes its fragment, which
  /// [`Self::guest_link_fragment`] returns once it is there.
  fn prepare_guest_link(&self, _conversation: ConversationId, _meeting: teamcraft_proto::MeetingId) {}
  /// What goes into the meeting link's `#fragment` (`v=<organizer fingerprint>`, as this device
  /// has the organizer's key pinned), once [`Self::prepare_guest_link`] made it.
  fn guest_link_fragment(&self, _meeting: teamcraft_proto::MeetingId) -> Option<String> {
    None
  }
  /// An organizer's device lets an admitted guest's device into the chat's group, until
  /// `access_ends_at_ms`, if the key it pins has the fingerprint the guest's page reported
  /// (e2ee.md §4.6, "Meeting guests").
  fn let_guest_into_meeting_chat(
    &self,
    _conversation: ConversationId,
    _guest: UserId,
    _knock_fingerprint: String,
    _access_ends_at_ms: i64,
  ) {
  }
  /// Pins a waiting guest's key and makes its key code, which [`Self::guest_key_code`] returns.
  fn prepare_guest_key_code(&self, _guest: UserId) {}
  /// The key code of a waiting guest's key as this device pinned it, for the lobby entry.
  fn guest_key_code(&self, _guest: UserId) -> Option<String> {
    None
  }
  /// Keeps the lobby setting the organizer chose for a meeting on this device.
  fn note_meeting_lobby(&self, _meeting: teamcraft_proto::MeetingId, _has_lobby: bool) {}
  /// Whether the organizer chose "no lobby" for the meeting on this device: only then does it let
  /// guests of the link in by itself.
  fn is_open_by_own_record(&self, _meeting: teamcraft_proto::MeetingId) -> bool {
    false
  }
  /// Unconfirmed removals in every conversation, for the admin console: an admin outside the
  /// group doesn't see the conversation.
  fn unconfirmed_removals(&self) -> Vec<(ConversationId, UserId)> {
    Vec::new()
  }
  /// Users still in an encryption group of this device whom the server no longer lists, confirmed
  /// by fresh member lists and with no signed removal on its way: for the admin console only,
  /// never shown to members (#661).
  fn group_leftovers(&self) -> Vec<(ConversationId, UserId)> {
    Vec::new()
  }

  fn first_unread(&self, _conversation: ConversationId) -> Option<MessageId> {
    None
  }
  /// Reactions of others to the user's own messages here that the user hasn't seen yet
  /// (notifications.md, "Reactions to my messages"). Read from an index, not the messages.
  fn unseen_reactions(&self, _conversation: ConversationId) -> crate::model::UnseenReactions {
    crate::model::UnseenReactions::default()
  }
  /// The user saw the reactions to these messages (they were in view, or the user jumped to
  /// them). Synced to the user's devices; never taken back.
  fn mark_reactions_seen(&self, _conversation: ConversationId, _messages: Vec<MessageId>) {}
  /// The newest message the server stored in the conversation, reactions, edits and deletions
  /// included: where `mark_read` moves the read marker. Unlike the newest row it changes when a
  /// reaction arrives, which raises the unread count too (#641).
  fn newest_message(&self, _conversation: ConversationId) -> Option<MessageId> {
    None
  }
  /// The unsent text of a conversation or thread (synced to the user's devices, encrypted).
  fn draft(&self, _conversation: ConversationId, _maybe_thread: Option<ThreadId>) -> Option<String> {
    None
  }
  /// Stores the draft; an empty body removes it. Call debounced while typing.
  fn set_draft(&self, _conversation: ConversationId, _maybe_thread: Option<ThreadId>, _body: &str) {}
  /// Starts (or finds) the direct conversation with a person; it shows up in the lists when ready.
  fn open_dm(&self, _user: UserId) {}
  /// Starts (or finds) the group DM with exactly these people and the user.
  fn open_group_dm(&self, _users: Vec<UserId>) {}
  /// Confirms having read a message that asked for acknowledgement.
  fn acknowledge(&self, _conversation: ConversationId, _message: MessageId) {}
  /// Sends an own message again that the server rejected.
  fn retry_send(&self, _conversation: ConversationId, _message: MessageId) {}
  /// Drops an own message the server rejected, instead of sending it again.
  fn discard_unsent(&self, _conversation: ConversationId, _message: MessageId) {}
  /// Stops uploading one file of an own message that is still going up (`file_id` from its
  /// [`crate::actions::FileInfo`]): the message goes out without it, or not at all when nothing is
  /// left of it.
  fn cancel_upload(&self, _conversation: ConversationId, _message: MessageId, _file_id: &str) {}

  // Files.

  /// Files of a conversation, newest first.
  fn conversation_files(&self, _conversation: ConversationId) -> Vec<FileInfo> {
    Vec::new()
  }
  /// The newest `limit` links shared in a conversation, newest first. The default reads the
  /// messages newest first and stops once `limit` links are found, so a long conversation is not
  /// read whole.
  fn conversation_links(&self, conversation: ConversationId, limit: usize) -> crate::links::LinkPage {
    crate::links::links_page(self, conversation, limit)
  }
  /// A page of at most `max_messages` messages with pictures or videos at `anchor`, newest first
  /// (the image viewer pages through a conversation with it, #274). Bounded work: a core reads
  /// only what the page holds. The default pages the whole list. `None` while a backend that asks
  /// the core waits for its answer (a change notification follows), so the viewer never mistakes
  /// a page not read yet for an empty one (#637).
  fn conversation_media(
    &self,
    conversation: ConversationId,
    anchor: MediaAnchor,
    max_messages: usize,
  ) -> Option<MediaPage> {
    Some(MediaPage::from_files(
      self.conversation_files(conversation),
      anchor,
      max_messages,
    ))
  }
  /// Fetches and decrypts a file; its [`FileInfo::transfer`] reports progress and the local path.
  fn download_file(&self, _conversation: ConversationId, _message: MessageId) {}
  /// Like [`Self::download_file`], for one file of the message (`file_id` of [`FileInfo`]).
  fn download_attachment(&self, conversation: ConversationId, message: MessageId, _file_id: &str) {
    self.download_file(conversation, message);
  }
  /// Creates a public download link for one file of a message (`file_id` of [`FileInfo`],
  /// e2ee.md §4.5.1); it appears in [`Self::share_links`].
  fn create_share_link(
    &self,
    _conversation: ConversationId,
    _message: MessageId,
    _file_id: &str,
    _options: ShareLinkOptions,
  ) {
  }
  /// The share links of one file of a message.
  fn share_links(
    &self,
    _conversation: ConversationId,
    _message: MessageId,
    _file_id: &str,
  ) -> Vec<ShareLink> {
    Vec::new()
  }
  /// Stops the share link `share_id` of one file from working.
  fn revoke_share_link(
    &self,
    _conversation: ConversationId,
    _message: MessageId,
    _file_id: &str,
    _share_id: &str,
  ) {
  }
  /// Asks the server for the current download counts of the file's links (they show in
  /// [`Self::share_links`] once the answer is in); links revoked elsewhere or expired drop out.
  fn refresh_share_links(&self, _conversation: ConversationId, _message: MessageId, _file_id: &str) {}
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
  /// Open spaces of the workspace the user is not in (members only; guests are invited).
  fn joinable_spaces(&self) -> Vec<crate::model::JoinableSpace> {
    Vec::new()
  }
  fn join_space(&self, _space: SpaceId) {}
  fn leave_channel(&self, _conversation: ConversationId) {}
  fn categories(&self, _space: SpaceId) -> Vec<CategoryInfo> {
    Vec::new()
  }
  fn create_category(&self, _space: SpaceId, _name: &str) {}
  fn rename_category(&self, _space: SpaceId, _category: CategoryId, _name: &str) {}
  fn delete_category(&self, _space: SpaceId, _category: CategoryId) {}
  /// Adds a person with an explicit history choice ("share history" toggle, e2ee.md §4.3).
  fn add_member_with_history(&self, _conversation: ConversationId, _user: UserId, _history: HistoryPolicy) {}
  /// Replaces the conversation's bookmarks bar (links and files shown under the header).
  fn set_bookmarks(&self, _conversation: ConversationId, _bookmarks: Vec<crate::model::Bookmark>) {}
  /// Removes a person from a conversation (and rotates its keys).
  fn remove_member(&self, _conversation: ConversationId, _user: UserId) {}
  /// Invites someone by email to a space, as member or guest.
  fn invite_to_space(&self, _invite: SpaceInvite) {}
  /// Adds people from the directory to a space as members or guests; people already in it are
  /// skipped.
  fn add_space_members(&self, _space: SpaceId, _users: Vec<UserId>, _is_guest: bool) {}
  /// Adds people to a channel (`(user, is_guest_account)`), first to its space when `joins_space`;
  /// someone whose space add failed is not added to the channel.
  fn add_people_to_channel(
    &self,
    _conversation: ConversationId,
    _people: Vec<(UserId, bool)>,
    _history: HistoryPolicy,
    _joins_space: bool,
  ) {
  }
  /// The channel's integration-bridge sources in force, oldest first (docs/bridge.md, "Sources").
  fn integration_sources(&self, _conversation: ConversationId) -> Vec<crate::model::IntegrationSource> {
    Vec::new()
  }
  /// Whether sources the user adds would be in force: they need an admin of the verified
  /// workspace roster (signed channel roles don't count for sources yet, e2ee.md §13.1).
  fn can_manage_integration_sources(&self) -> bool {
    false
  }
  /// Announces a new source in the channel: an encrypted, hidden control message the bridge reads.
  fn add_integration_source(
    &self,
    _conversation: ConversationId,
    _source: crate::model::NewIntegrationSource,
  ) {
  }
  /// Revokes a source for good: its token stops working once the bridge reads the revocation.
  fn revoke_integration_source(&self, _conversation: ConversationId, _source: teamcraft_proto::SourceId) {}
  /// The notetaker group of the verified workspace policy (e2ee.md §8.2); `None` before a policy
  /// was verified. The call screen shows the notetaker from this, never from the server's word.
  fn notetaker_policy(&self) -> Option<teamcraft_proto::NotetakerPolicy> {
    None
  }
  /// Asks the notetaker into the conversation's call or sends it away: an encrypted, hidden
  /// control message the notetaker reads (and decides on from the verified policy).
  fn request_notetaker(&self, _conversation: ConversationId, _request: teamcraft_proto::NotetakerRequest) {}
  /// The channel's members with their roles; empty until the member list is loaded.
  fn channel_roles(&self, _conversation: ConversationId) -> Vec<crate::model::ChannelMember> {
    Vec::new()
  }
  /// Gives a channel member another role. The UI offers only what [`crate::role_rules`] allows;
  /// the server enforces the permission and its invariants, and a refusal shows as a failed
  /// action. The core then signs the channel's new roles where the user can (issue #380).
  fn set_channel_role(&self, _conversation: ConversationId, _user: UserId, _role: crate::model::ChannelRole) {
  }
  /// Signs the role the server lists for a member, whose role is the server's word only
  /// ([`crate::model::ChannelMember::is_role_signed`]); nothing changes on the server.
  fn confirm_channel_role(&self, _conversation: ConversationId, _user: UserId) {}
  /// Reads the channel's members and signed roles afresh (the members section opened).
  fn refresh_channel_roles(&self, _conversation: ConversationId) {}
  /// Whether the user can sign the channel's roles in a way members accept: a signed channel
  /// admin or an admin of the signed workspace roster.
  fn can_sign_channel_roles(&self, _conversation: ConversationId) -> bool {
    false
  }
  /// The members of a space with their roles, as far as the user may see them.
  fn space_members(&self, _space: SpaceId) -> crate::model::SpaceRoster {
    crate::model::SpaceRoster::Loading
  }
  /// The space's pending e-mail invitations (empty for people who may not manage members).
  fn space_invites(&self, _space: SpaceId) -> Vec<crate::model::PendingInvite> {
    Vec::new()
  }
  /// Loads the members and pending invitations of a space afresh (the space settings opened).
  fn refresh_space_roster(&self, _space: SpaceId) {}
  fn set_space_role(&self, _space: SpaceId, _user: UserId, _role: crate::model::SpaceRole) {}
  /// Removes someone from the space and, on the server, from its channels.
  fn remove_space_member(&self, _space: SpaceId, _user: UserId) {}
  fn revoke_space_invite(&self, _space: SpaceId, _invite: crate::model::InviteId) {}
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

  // Activity feed, last seen and call history.

  /// Mentions, reactions to the user's messages and replies in their threads, newest first, built
  /// from the decrypted local data.
  fn activity(&self) -> ActivityFeed {
    ActivityFeed::default()
  }
  /// The user looked at the feed: everything in it so far counts as read (synced to their devices).
  fn mark_activity_read(&self) {}
  /// The admins' requests waiting for the user's decision, as in [`Self::activity`]; backends that
  /// build the feed locally answer without building it (the notifications ask on every change).
  fn join_requests(&self) -> Vec<crate::activity::JoinRequest> {
    self.activity().join_requests
  }
  /// When the person was last active, if they are offline and let others see it. Backends that
  /// read the server ask for it on first look and answer `None` until it arrives.
  fn last_seen(&self, _user: UserId) -> Option<TimestampMs> {
    None
  }
  /// The user's own privacy choices.
  fn privacy(&self) -> PrivacySettings {
    PrivacySettings::default()
  }
  fn set_last_seen_visibility(&self, _visibility: LastSeenVisibility) {}
  /// The user's calls, newest first. Backends that read the server fetch them on first look.
  fn call_history(&self) -> Vec<CallHistoryItem> {
    Vec::new()
  }
  /// Asks for the newest calls again (the call list opened).
  fn refresh_call_history(&self) {}
  /// Whether the list has had its first answer. Until then the UI shows placeholders, not the
  /// empty state; a backend that reads everything locally is always loaded.
  fn is_loaded(&self, _list: LoadingList) -> bool {
    true
  }

  /// This user's devices as the server last listed them; empty until the first answer.
  fn own_devices(&self) -> Vec<crate::admin::DeviceInfo> {
    Vec::new()
  }

  /// The device list was opened: fetch it again.
  fn refresh_own_devices(&self) {}

  /// Signs out one of the user's other devices (e2ee.md §3: the signed device list drops it).
  fn revoke_own_device(&self, _device: DeviceId) {}

  /// Renames one of the user's devices; the list shows the new name after the next answer.
  fn rename_own_device(&self, _device: DeviceId, _name: String) {}

  /// Sends a test push to one of the user's devices; the outcome shows in the device list.
  fn test_push_own_device(&self, _device: DeviceId) {}
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

  /// Whether this client can take part in calls at all. Without call support the UI offers no call
  /// buttons instead of a call screen that connects nowhere.
  fn has_calls(&self) -> bool {
    false
  }

  /// Why [`CallBackend::has_calls`] is `false`, for the dialog the call buttons open then.
  fn calls_unavailable_reason(&self) -> CallsUnavailableReason {
    CallsUnavailableReason::NotInThisApp
  }

  /// Calls are a simulation (the development demo): speaking, levels and codes are made up.
  fn has_simulated_calls(&self) -> bool {
    false
  }

  /// Which background effects the camera image can get here (#439). Without any, the UI offers
  /// none.
  fn effects_offer(&self) -> crate::calls::EffectsOffer {
    crate::calls::EffectsOffer::Full
  }

  /// Counts joins that failed since the app started (a refused microphone, an unreachable call
  /// server), so the UI can leave the call screen it opened for them.
  fn call_join_failures(&self) -> u64 {
    0
  }

  /// Whether call actions the user took are still being carried out. Until they are, the UI keeps
  /// its own idea of the camera and screen share instead of the media layer's.
  fn has_pending_call_actions(&self) -> bool {
    false
  }

  /// Others' video in the active call.
  fn call_video_tracks(&self) -> Vec<RemoteVideoTrack> {
    Vec::new()
  }

  /// Reads what the active call's video does (stats for nerds, #938) and hands it to `done`
  /// once the media layer answered; `None` outside a call or where the media layer can't tell.
  fn fetch_call_video_stats(&self, done: Box<dyn FnOnce(Option<VideoStatsJson>)>) {
    done(None);
  }

  /// The call this device is in.
  fn active_call(&self) -> Option<ActiveCall> {
    None
  }
  /// Why the last call ended other than by leaving (the other side of a 1:1 call left, #997).
  fn call_end_reason(&self) -> Option<CallEndReason> {
    None
  }
  /// Calls ringing for the user that they haven't answered or declined.
  fn incoming_calls(&self) -> Vec<IncomingCall> {
    Vec::new()
  }
  /// Joins (or starts) the conversation's call, leaving any other call first. The media starts as
  /// `media` says: a muted microphone sends nothing before an unmute, a chosen camera starts with
  /// the call.
  fn join_call(&self, _request: CallRequest, _media: JoinMedia) {}
  fn leave_call(&self) {}
  /// Stops ringing for this call on this device; the call goes on for the others.
  fn decline_call(&self, _conversation: ConversationId) {}
  fn set_call_muted(&self, _is_muted: bool) {}
  fn set_call_deafened(&self, _is_deafened: bool) {}
  /// Takes the active call over from the account's other devices, which leave it (#505).
  fn move_call_here(&self) {}
  /// Takes all of the user's devices out of the conversation's call ("Leave on all devices"),
  /// also one only the server still lists (#684).
  fn leave_call_everywhere(&self, _conversation: ConversationId) {}
  /// Which call status sounds play on this device, for this and later calls.
  fn set_call_status_sounds(&self, _sounds: CallStatusSounds) {}
  /// The highest quality the own camera is sent at, for this and later calls (#967); a running
  /// camera restarts with it.
  fn set_camera_quality(&self, _quality: crate::calls::VideoQuality) {}
  /// How sharp another device's track comes in the active call (#967): Auto by its tile, else at
  /// most the lines picked.
  fn set_video_quality(
    &self,
    _stream_id: teamcraft_proto::MediaStreamId,
    _quality: crate::calls::VideoQuality,
  ) {
  }
  /// A [`crate::calls::MediaDevice::id`]; `None` follows the system default.
  fn select_microphone(&self, _maybe_device: Option<String>) {}
  fn select_speakers(&self, _maybe_device: Option<String>) {}
  /// Lists the devices again; [`CallBackend::media_devices`] changes after a notification.
  fn refresh_media_devices(&self) {}
  /// Plays a short test sound on the speakers; a failure reaches the user.
  fn test_speakers(&self, _maybe_device: Option<String>) {}
  fn set_call_camera(&self, _is_on: bool) {}
  fn select_camera(&self, _maybe_device: Option<String>) {}
  /// The camera that stood in for the chosen one at the last camera start, if one did (#819).
  fn camera_fallback(&self) -> Option<CameraFallback> {
    None
  }
  fn set_background_effect(&self, _effect: BackgroundEffect) {}
  /// Opens the system's video effects panel (macOS); nothing elsewhere.
  fn open_system_video_effects(&self) {}
  /// `has_sound`: share the computer's sound too (without the call's own).
  fn start_screen_share(
    &self,
    _has_sound: bool,
    _mode: ScreenContentMode,
    _resolution: ScreenShareResolution,
  ) {
  }
  fn set_push_to_talk(&self, _is_enabled: bool, _maybe_shortcut: Option<String>) {}
  /// The push-to-talk key inside the window went down (`true`) or up.
  fn set_talking(&self, _is_talking: bool) {}
  fn stop_screen_share(&self) {}
  /// Starts, changes or ends the pre-join check (#442): the camera for the self view, the
  /// microphone for [`CallBackend::call_preview_level`]. Nothing of it leaves the device.
  fn set_call_preview(&self, _preview: CallPreview) {}
  /// How far the pre-join check got with the camera.
  fn call_preview_camera(&self) -> PreviewCamera {
    PreviewCamera::Off
  }
  /// The microphone's level in the pre-join check, or in the running call while
  /// [`CallBackend::watch_call_level`] is on, from 0.0 (silence) to 1.0 (full scale); the meter
  /// reads it a few times a second.
  fn call_preview_level(&self) -> f32 {
    0.0
  }
  /// The call's device menu shows its meter (#986): the running call's microphone level comes
  /// through [`CallBackend::call_preview_level`] until it is turned off again.
  fn watch_call_level(&self, _is_watching: bool) {}
}

/// Administration without content access: every read returns metadata only.
pub trait AdminBackend {
  fn my_role(&self) -> Role;
  fn admin_channels(&self) -> Vec<AdminChannel>;
  fn admin_users(&self) -> Vec<AdminUser>;
  /// The signed workspace policy in force and its version (0: none was signed yet).
  fn workspace_policy(&self) -> (u64, WorkspacePolicy);
  /// Signs and stores a new policy version. Only owners with signing keys can. `genesis` is the
  /// first roster the owner confirmed ([`AdminBackend::admin_genesis_roster`]); it is signed only
  /// while the workspace has no signed roster and ignored afterwards.
  fn set_workspace_policy(&self, policy: WorkspacePolicy, genesis: Vec<crate::admin::RosterEntry>);
  fn set_channel_retention(&self, channel: ConversationId, retention: Retention);
  fn remove_device(&self, user: UserId, device: DeviceId);
  /// Suspends (`false`) or reactivates (`true`) an account; memberships and devices stay.
  fn set_user_active(&self, user: UserId, is_active: bool);
  /// Removes a deactivated user from all chats: this admin's client signs the removal from every
  /// channel (members commit it, keys rotate), then devices, direct messages and spaces go. Not
  /// undone by a reactivation. Does nothing without [`AdminBackend::can_remove_from_chats`].
  fn remove_user_from_chats(&self, user: UserId);
  /// Whether this client can sign the removals of "Remove from all chats" (it holds the device
  /// keys of an admin); without, the console explains instead of offering a server-only removal.
  fn can_remove_from_chats(&self) -> bool {
    false
  }
  /// Deletes an account (admin.md §3.2.1): this admin's client signs the removal from every
  /// channel first, then the server deletes devices, key material, memberships and direct
  /// messages; a "Deleted user" tombstone stays on old messages. Needs
  /// [`AdminBackend::can_remove_from_chats`].
  fn delete_user(&self, _user: UserId) {}
  /// Removes a user left over in the encryption group of a conversation (issue #734), signed and
  /// committed by this admin's client. Needs [`AdminBackend::can_remove_from_chats`].
  fn remove_group_leftover(&self, _conversation: ConversationId, _user: UserId) {}
  fn start_org_recovery(&self, user: UserId);
  /// Whether [`AdminBackend::start_org_recovery`] does anything on this client.
  fn can_start_org_recovery(&self) -> bool {
    false
  }
  /// Adds the admin to a private channel according to the `admin_self_join` policy.
  fn admin_join_channel(&self, channel: ConversationId) -> SelfJoinOutcome;
  fn audit_log(&self) -> Vec<AuditEntry>;

  /// Called when the console opens: backends that read the server fetch fresh data now.
  fn admin_refresh(&self) {}

  /// Spaces by name, for the permission scheme scopes.
  fn admin_spaces(&self) -> Vec<AdminSpace> {
    Vec::new()
  }
  /// Whether the space and channel lists arrived (`admin_refresh` retries a failed load).
  fn admin_load(&self) -> AdminLoad {
    AdminLoad::Loaded
  }
  /// Renames, archives or restores a space, or changes how people join it. The server decides who
  /// may; the admin needs no membership and sees no content.
  fn change_admin_space(&self, _space: SpaceId, _changes: AdminSpaceChanges) {}
  /// What each role may do at the scope (`None`: the whole instance) and the changes made there.
  fn permission_scheme(&self, _maybe_space: Option<SpaceId>) -> PermissionSchemeResponse {
    PermissionSchemeResponse { roles: Vec::new() }
  }
  fn change_permissions(&self, _maybe_space: Option<SpaceId>, _changes: Vec<SchemeChange>) {}
  /// The permissions a channel takes away from its members and guests.
  fn channel_moderation(&self, _channel: ConversationId) -> ChannelModeration {
    ChannelModeration::default()
  }
  fn set_channel_moderation(&self, _channel: ConversationId, _moderation: ChannelModeration) {}
  /// People who can become the owner of the channel (its members).
  fn channel_member_candidates(&self, _channel: ConversationId) -> Vec<UserId> {
    Vec::new()
  }
  fn assign_channel_owner(&self, _channel: ConversationId, _user: UserId) {}
  /// The announcement banner in force, if any.
  fn announcement(&self) -> Option<AnnouncementResponse> {
    None
  }
  fn set_announcement(&self, _announcement: SetAnnouncementRequest) {}
  fn clear_announcement(&self) {}
  /// Media server, push providers and storage; `None` while unknown.
  fn instance_status(&self) -> Option<AdminInstanceResponse> {
    None
  }
  fn audit_filter(&self) -> AuditFilter {
    AuditFilter::default()
  }
  /// Narrows the audit log; backends that read the server reload it.
  fn set_audit_filter(&self, _filter: AuditFilter) {}
  /// Why the signed policy cannot be changed here (not an owner, no encryption keys on this device).
  fn policy_edit_blocker(&self) -> Option<crate::admin::PolicyBlocker> {
    None
  }
  /// The owners of the verified roster and when each last signed, for the owner warnings.
  fn admin_owner_roster(&self) -> Option<crate::admin::OwnerRosterStatus> {
    None
  }
  /// The identity provider's role holders as the server reports them: (owners, admins).
  fn admin_role_holders(&self) -> (Vec<UserId>, Vec<UserId>) {
    (Vec::new(), Vec::new())
  }
  /// Signs `members` as the next roster on this device (owners only), unless the verified roster
  /// is no longer version `base_version`.
  fn admin_publish_roster(&self, _base_version: u64, _members: Vec<crate::admin::RosterEntry>) {}
  /// The first roster to confirm before the first policy is signed, while this device verified
  /// no roster yet (#265): the signer and the identity provider's role holders. `None` once a
  /// roster is verified; later changes go through [`Self::admin_publish_roster`].
  fn admin_genesis_roster(&self) -> Option<Vec<crate::admin::RosterEntry>> {
    None
  }
  /// The identity keys this device pinned, to show what an owner is about to vouch for.
  fn admin_identity_keys(&self) -> crate::remote::SecuritySnapshot {
    crate::remote::SecuritySnapshot::default()
  }

  // ---- Runtime settings (admin.md §6): brands, mail, calendar, instance settings ----

  /// Every brand and the host names the server is deployed for; `None` while unknown.
  fn admin_brands(&self) -> Option<AdminBrandsResponse> {
    None
  }
  fn create_brand(&self, _request: CreateBrandRequest) {}
  fn update_brand(&self, _id: String, _request: UpdateBrandRequest) {}
  fn delete_brand(&self, _id: String) {}
  /// Copies a brand of the configuration file into the database, where it can be edited.
  fn take_over_brand(&self, _id: String) {}
  /// A `data:` URL of the brand image once it is loaded (the request starts on the first call).
  fn brand_asset_preview(&self, _id: &str, _slot: &BrandAssetSlot) -> Option<String> {
    None
  }
  fn upload_brand_asset(&self, _id: String, _slot: BrandAssetSlot, _bytes: Vec<u8>, _content_type: String) {}
  fn delete_brand_asset(&self, _id: String, _slot: BrandAssetSlot) {}
  fn mail_settings(&self) -> Option<AdminMailSettingsResponse> {
    None
  }
  fn save_mail_settings(&self, _request: UpdateMailSettingsRequest) {}
  /// Back to the values of the configuration file.
  fn reset_mail_settings(&self) {}
  fn send_test_mail(&self, _request: SendTestMailRequest) {}
  fn calendar_settings(&self) -> Option<AdminCalendarSettingsResponse> {
    None
  }
  fn save_calendar_settings(&self, _request: UpdateCalendarSettingsRequest) {}
  fn reset_calendar_settings(&self) {}
  fn test_calendar(&self) {}
  /// The answer of the last mail or calendar test of this console session.
  fn last_settings_test(&self) -> Option<(SettingsTestKind, SettingsTestResponse)> {
    None
  }
  /// Whether the notetaker's OpenRouter key is stored (admin.md §3.7); `None` until loaded.
  fn notetaker_settings(&self) -> Option<teamcraft_proto::AdminNotetakerResponse> {
    None
  }
  /// Stores, replaces or removes the notetaker's key at the server (write-only).
  fn set_notetaker_key(&self, _key: teamcraft_proto::SecretUpdate) {}
  fn runtime_settings(&self) -> Option<RuntimeSettingsResponse> {
    None
  }
  fn save_runtime_settings(&self, _section: RuntimeSettingsSection) {}
  fn reset_runtime_settings(&self, _key: RuntimeSettingsKey) {}

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
  /// The device list was opened: ask the server again.
  fn refresh_my_devices(&self) {}
  fn rename_my_device(&self, _device: DeviceId, _name: String) {}
  fn test_push_my_device(&self, _device: DeviceId) {}
  /// The key backup as this device knows it; `None` without encryption keys (or before the core
  /// reported it).
  fn key_backup(&self) -> Option<crate::security::KeyBackupState> {
    None
  }
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

/// The people whose display name or username contains `query` (any case), sorted by name.
pub fn filter_people(mut people: Vec<User>, query: &str) -> Vec<User> {
  let needle = query.trim().to_lowercase();
  people.retain(|user| {
    // Nobody picks a deactivated account (issue #654).
    !user.is_deactivated
      && (needle.is_empty()
        || user.display_name.to_lowercase().contains(&needle)
        || user.username.to_lowercase().contains(&needle))
  });
  people.sort_by_key(|user| (user.display_name.to_lowercase(), user.id));
  people
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{DeviceSetup, PresenceStatus, id_from_raw};

  #[test]
  fn deactivated_accounts_are_never_offered() {
    let people = vec![
      person(1, "Alice", "alice", false),
      person(2, "Alice (old)", "alice-old", true),
    ];
    let found = filter_people(people.clone(), "alice");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].username, "alice");
    assert_eq!(filter_people(people, "").len(), 1);
  }

  fn person(id: u64, display_name: &str, username: &str, is_deactivated: bool) -> User {
    User {
      id: id_from_raw(id),
      username: username.into(),
      display_name: display_name.into(),
      presence: PresenceStatus::Offline,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      is_deactivated,
      is_deleted: false,
      device_setup: DeviceSetup::Unknown,
      maybe_email: None,
    }
  }
}
