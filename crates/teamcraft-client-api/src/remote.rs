//! Wire types between a client core running outside the UI (the desktop shell's Rust side) and the
//! UI in the webview. The core owns the encrypted store and the network; the UI keeps a mirror of
//! what it shows and asks for more through these messages.

use serde::{Deserialize, Serialize};

use teamcraft_proto::{BackgroundId, Base64Bytes, CategoryId, HistoryPolicy, OrgBackgroundId};

use crate::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, FileInfo, MessageExtras, NewChannel, NewSpace,
  OutgoingMessage, PermalinkTarget, SavedItem, ShareLink, ShareLinkOptions, SpaceChanges, SpaceInvite,
};
use crate::activity::ActivityFeed;
use crate::admin::DeviceInfo;
use crate::backend::{ChatBackend, SearchBackend};
use crate::calls::{
  ActiveCall, BackgroundEffect, BackgroundImageInfo, CallEndReason, CallHistoryItem, CallRequest,
  CallStatusSounds, IncomingCall, MediaDevices, OrgBackgroundRule, OrgBackgrounds, PushToTalk,
  RemoteVideoTrack, RunningCall,
};
use crate::live::LiveSnapshot;
use crate::model::{
  Conversation, ConversationId, CustomStatus, DeviceId, MembershipIssue, Message, MessageId, PresenceStatus,
  Space, SpaceId, ThreadId, ThreadSummary, TimestampMs, User, UserId,
};
use crate::privacy::{LastSeenVisibility, PrivacySettings};
use crate::search::{SavedSearch, SearchHit, SearchResults};

/// Event name the host emits whenever the local store changed (new messages, membership, presence).
pub const CORE_CHANGED_EVENT: &str = "core-changed";

/// Event name the host emits whenever call state changed (ringing, roster, media).
pub const CALL_CHANGED_EVENT: &str = "call-changed";

/// A `teamcraft:` link the operating system handed to the desktop app; the payload is the link.
pub const DEEP_LINK_EVENT: &str = "teamcraft://deep-link";

/// Everything the call UI shows, in one piece.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CallSnapshot {
  #[serde(default)]
  pub maybe_active: Option<ActiveCall>,
  pub incoming: Vec<IncomingCall>,
  /// Calls running in the user's conversations.
  pub running: Vec<RunningCall>,
  pub devices: MediaDevices,
  /// Video of others in the active call.
  #[serde(default)]
  pub video_tracks: Vec<RemoteVideoTrack>,
  #[serde(default)]
  pub push_to_talk: PushToTalk,
  /// Set when the meeting host removed the user or ended the call; cleared on the next join.
  #[serde(default)]
  pub maybe_end_reason: Option<CallEndReason>,
}

/// Something the user did in or about a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CallAction {
  Join {
    request: CallRequest,
  },
  Leave,
  Decline {
    conversation: ConversationId,
  },
  SetMuted {
    is_muted: bool,
  },
  SetDeafened {
    is_deafened: bool,
  },
  /// `None` follows the system default.
  SelectMicrophone {
    #[serde(default)]
    maybe_device: Option<String>,
  },
  SelectSpeakers {
    #[serde(default)]
    maybe_device: Option<String>,
  },
  SetCamera {
    is_on: bool,
  },
  /// `None` follows the system default (or the portal's choice).
  SelectCamera {
    #[serde(default)]
    maybe_device: Option<String>,
  },
  SetBackgroundEffect {
    effect: BackgroundEffect,
  },
  /// Push-to-talk on or off; `maybe_shortcut` changes the shortcut (desktop).
  SetPushToTalk {
    is_enabled: bool,
    #[serde(default)]
    maybe_shortcut: Option<String>,
  },
  /// The push-to-talk key went down or up inside the window (browser, or desktops without global
  /// shortcuts).
  SetTalking {
    is_talking: bool,
  },
  /// Asks the system for a screen or window and shares it as its own stream, with the computer's
  /// sound when `has_sound` (older UIs send no flag: picture only).
  StartScreenShare {
    #[serde(default)]
    has_sound: bool,
  },
  StopScreenShare,
  /// Opens the system's video effects panel (macOS: Portrait, background, Studio Light).
  OpenSystemVideoEffects,
  /// Which call status sounds play (a device setting; also before any call).
  SetStatusSounds {
    sounds: CallStatusSounds,
  },
  #[serde(other)]
  Unknown,
}

/// HTTP verbs the UI may use for authenticated server calls (the shell attaches the bearer token).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiMethod {
  Get,
  Post,
  Put,
  Patch,
  Delete,
}

impl ApiMethod {
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Get => "GET",
      Self::Post => "POST",
      Self::Put => "PUT",
      Self::Patch => "PATCH",
      Self::Delete => "DELETE",
    }
  }
}

/// Everything the sidebar and headers need, in one piece.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChatSnapshot {
  pub me: User,
  pub users: Vec<User>,
  pub spaces: Vec<Space>,
  /// Channels of all spaces plus direct and group messages.
  pub conversations: Vec<Conversation>,
  /// Top-level messages per conversation, so the feed knows its length before loading any row.
  pub message_counts: Vec<(ConversationId, usize)>,
  /// When the newest top-level message of each conversation was sent (conversations without one
  /// are left out), so lists can be ordered by recency.
  #[serde(default)]
  pub last_activity: Vec<(ConversationId, TimestampMs)>,
  /// Identity keys this device knows, for key verification and key-change warnings.
  #[serde(default)]
  pub security: SecuritySnapshot,
  /// Server-reported counters the core keeps from gateway events; absent from older shells.
  #[serde(rename = "live", default, skip_serializing_if = "Option::is_none")]
  pub maybe_live: Option<LiveSnapshot>,
}

/// Fingerprints (hex) of the identity keys on this device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecuritySnapshot {
  pub my_fingerprint: String,
  pub contacts: Vec<ContactKeys>,
  /// What notifications may show, from the policy this device verified. `None` (not known yet, or
  /// an older shell) is treated as the strictest mode.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_push_content: Option<teamcraft_proto::PushContentMode>,
  /// The owners of the verified roster, for the admin console's warnings; `None` before a roster
  /// was verified (or from an older shell).
  #[serde(rename = "owners", default, skip_serializing_if = "Option::is_none")]
  pub maybe_owners: Option<crate::admin::OwnerRosterStatus>,
}

/// What this device pinned for one contact, and a different key the server has shown since.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactKeys {
  pub user: UserId,
  pub fingerprint: String,
  /// Set when the server now presents another identity key (e2ee.md §3: shown, never trusted silently).
  #[serde(default)]
  pub maybe_new_fingerprint: Option<String>,
  /// The identity provider attested the pinned key (otherwise it was trusted on first use).
  #[serde(default)]
  pub is_attested: bool,
}

impl ChatSnapshot {
  /// Reads the snapshot from a backend; `security` comes from the host (it knows the pins).
  pub fn read(backend: &impl ChatBackend, security: SecuritySnapshot) -> Self {
    let spaces = backend.spaces();
    let mut conversations = Vec::new();
    for space in &spaces {
      conversations.extend(backend.space_conversations(space.id));
    }
    conversations.extend(backend.direct_conversations());
    let message_counts = conversations
      .iter()
      .map(|conversation| (conversation.id, backend.message_count(conversation.id)))
      .collect();
    let last_activity = last_activity_of(backend, &conversations);
    Self {
      me: backend.me(),
      users: backend.users(),
      spaces,
      conversations,
      message_counts,
      last_activity,
      security,
      maybe_live: backend.live_snapshot(),
    }
  }
}

/// The newest message time of every conversation that has one.
pub fn last_activity_of(
  backend: &(impl ChatBackend + ?Sized),
  conversations: &[Conversation],
) -> Vec<(ConversationId, TimestampMs)> {
  conversations
    .iter()
    .filter_map(|conversation| {
      backend
        .last_activity(conversation.id)
        .map(|at| (conversation.id, at))
    })
    .collect()
}

impl MessageWindow {
  /// `len` messages of a conversation's feed from `start` on.
  pub fn read(backend: &impl ChatBackend, conversation: ConversationId, start: usize, len: usize) -> Self {
    Self {
      conversation,
      start,
      total: backend.message_count(conversation),
      messages: backend.messages(conversation, start, len),
    }
  }
}

/// Linking this (new) device to another one of the user (e2ee.md §5), as the shell reports it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LinkStatus {
  #[default]
  Idle,
  /// Showing the code (QR) and waiting for the other device to answer.
  WaitingForScan {
    code: String,
  },
  /// Both devices show the short code; the user confirms on the existing device.
  Confirm {
    code: String,
    short_code: String,
  },
  Failed {
    message: String,
  },
}

/// A slice of one conversation's feed, oldest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageWindow {
  pub conversation: ConversationId,
  pub start: usize,
  /// Length of the whole feed at the time of reading.
  pub total: usize,
  pub messages: Vec<Message>,
}

/// Where the local client stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CoreStatus {
  /// No signed-in account yet.
  NotStarted,
  /// First run on this device: keys have to be created or recovered.
  NeedsEnrollment {
    /// The account already has keys on the server, so the recovery passphrase unlocks them.
    has_existing_identity: bool,
    /// This device was set up before, but its local data is gone (the browser deleted it): the user
    /// is told why setup is asked for again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    was_data_lost: bool,
  },
  Ready {
    /// Fingerprint of the account's identity key.
    identity_fingerprint: String,
  },
  Failed {
    message: String,
  },
}

/// Where the core's gateway connection stands, for the connection banner (the core's
/// `ConnectionState` on its way to the webview).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CoreConnection {
  Connecting,
  Connected,
  /// Not connected; the next attempt starts at `retry_at_ms` (Unix milliseconds).
  Offline {
    retry_at_ms: i64,
  },
  /// The server refused the access token: the user has to sign in again.
  SignInRequired,
}

/// Whether the local data keeps up with the core. The browser can refuse writes (quota) or hand
/// the database to a newer version of the app (e2ee.md §9.4); the user is told, since a reload would
/// lose what was not saved.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreHealth {
  #[default]
  Saved,
  /// The browser has no storage space left for the app.
  QuotaExceeded,
  /// A newer version of the app took the database over: reload.
  Closed,
  /// Writes fail for another reason.
  Failed,
  #[serde(other)]
  Unknown,
}

/// First-run setup of this device (e2ee.md §3, §6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollRequest {
  /// Wraps the key backup; recovers an existing identity when the account has one.
  pub passphrase: String,
  /// Shown in the user's device list.
  pub device_name: String,
}

/// Something the user did that changes data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
// An action lives for one user gesture; boxing the policy would only add noise at every call site.
#[allow(clippy::large_enum_variant)]
pub enum ChatAction {
  PostMessage {
    conversation: ConversationId,
    #[serde(default)]
    maybe_thread: Option<ThreadId>,
    body: String,
  },
  ToggleReaction {
    conversation: ConversationId,
    message: MessageId,
    emoji: String,
  },
  MarkRead {
    conversation: ConversationId,
  },
  SetFavorite {
    conversation: ConversationId,
    is_favorite: bool,
  },
  SetPresence {
    presence: PresenceStatus,
    /// When a timed presence (quick do-not-disturb) ends; without it the presence stays.
    #[serde(default)]
    maybe_until: Option<TimestampMs>,
  },
  SetCustomStatus {
    #[serde(default)]
    maybe_status: Option<CustomStatus>,
  },
  /// Everything in the activity feed counts as read.
  MarkActivityRead,
  SetLastSeenVisibility {
    visibility: LastSeenVisibility,
  },
  RefreshCallHistory,
  /// Fetch this user's device list again (the devices settings were opened).
  RefreshOwnDevices,
  /// Sign out one of this user's other devices.
  RevokeOwnDevice {
    device: DeviceId,
  },
  /// Founds a new channel (and its encryption group) in a space.
  CreateChannel {
    space: SpaceId,
    name: String,
  },
  /// Opens, or finds, the direct message with one person.
  OpenDm {
    user: UserId,
  },
  /// Opens, or finds, the group DM with exactly these people (and the user).
  OpenGroupDm {
    users: Vec<UserId>,
  },
  /// Adds a person to a conversation and gives them the history the policy allows.
  AddMember {
    conversation: ConversationId,
    user: UserId,
  },
  /// Signs and publishes a workspace policy version. Owners and admins form the first signed roster
  /// when the workspace has none yet (the signer becomes the root of trust).
  PublishPolicy {
    /// The policy the editor started from; the core refuses to sign unless it is the one it verified.
    base: teamcraft_proto::WorkspacePolicy,
    policy: teamcraft_proto::WorkspacePolicy,
    #[serde(default)]
    owners: Vec<UserId>,
    #[serde(default)]
    admins: Vec<UserId>,
  },
  /// Signs and publishes the admin roster as the next version (owners only, admin.md §2.3). The
  /// core refuses a roster without an owner and a signer who isn't an owner.
  PublishRoster {
    /// The roster version the editor started from; the core signs nothing if a newer one arrived.
    base_version: u64,
    members: Vec<crate::admin::RosterEntry>,
  },
  /// Wraps the key backup with a new recovery passphrase (the old one stops working for recovery).
  ChangePassphrase {
    passphrase: String,
  },
  /// This device became idle (no input for a while) or active again; the server shows idle devices
  /// as away (docs/gateway.md).
  SetActivity {
    is_idle: bool,
  },
  /// Whether the user is looking at this session (window focused and visible, recent input, screen
  /// not locked); repeated while attentive (notifications.md, "Attention").
  SetAttention {
    is_attentive: bool,
  },
  /// The user compared the keys and accepts the contact's new identity key.
  AcceptKeyChange {
    user: UserId,
  },
  /// An admin adds themselves to a conversation (subject to the policy's `admin_self_join`).
  AdminJoinChannel {
    conversation: ConversationId,
  },
  // The actions of crate::actions; `apply_to` runs them on a backend.
  SendMessage {
    message: OutgoingMessage,
  },
  EditMessage {
    conversation: ConversationId,
    message: MessageId,
    body: String,
  },
  DeleteMessage {
    conversation: ConversationId,
    message: MessageId,
  },
  ForwardMessage {
    from: ConversationId,
    message: MessageId,
    to: ConversationId,
    #[serde(default)]
    comment: String,
  },
  SetPinned {
    conversation: ConversationId,
    message: MessageId,
    is_pinned: bool,
  },
  SetSaved {
    conversation: ConversationId,
    message: MessageId,
    is_saved: bool,
  },
  /// A picture file for the user's backgrounds (JPEG or PNG; the host normalizes it).
  AddBackgroundImage {
    name: String,
    picture: Base64Bytes,
  },
  DeleteBackgroundImage {
    id: BackgroundId,
  },
  /// Owners: an organization background (JPEG or PNG; the host normalizes it).
  AddOrgBackground {
    name: String,
    picture: Base64Bytes,
  },
  DeleteOrgBackground {
    id: OrgBackgroundId,
  },
  SetOrgBackgroundRule {
    rule: OrgBackgroundRule,
  },
  MarkUnread {
    conversation: ConversationId,
    from: MessageId,
  },
  SetDraft {
    conversation: ConversationId,
    #[serde(default)]
    maybe_thread: Option<ThreadId>,
    body: String,
  },
  Acknowledge {
    conversation: ConversationId,
    message: MessageId,
  },
  RetrySend {
    conversation: ConversationId,
    message: MessageId,
  },
  /// The user is typing (send while the text changes; the core throttles to one per few seconds).
  Typing {
    conversation: ConversationId,
  },
  DiscardUnsent {
    conversation: ConversationId,
    message: MessageId,
  },
  DownloadFile {
    conversation: ConversationId,
    message: MessageId,
  },
  DownloadAttachment {
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
  },
  CreateShareLink {
    conversation: ConversationId,
    message: MessageId,
    options: ShareLinkOptions,
  },
  CreateSpace {
    space: NewSpace,
  },
  UpdateSpace {
    space: SpaceId,
    changes: SpaceChanges,
  },
  CreateChannelWith {
    channel: NewChannel,
  },
  UpdateChannel {
    conversation: ConversationId,
    changes: ChannelChanges,
  },
  JoinChannel {
    conversation: ConversationId,
  },
  JoinSpace {
    space: SpaceId,
  },
  LeaveChannel {
    conversation: ConversationId,
  },
  CreateCategory {
    space: SpaceId,
    name: String,
  },
  RenameCategory {
    space: SpaceId,
    category: CategoryId,
    name: String,
  },
  DeleteCategory {
    space: SpaceId,
    category: CategoryId,
  },
  RemoveMember {
    conversation: ConversationId,
    user: UserId,
  },
  RetryRemoval {
    conversation: ConversationId,
    user: UserId,
  },
  DiscardRemoval {
    conversation: ConversationId,
    user: UserId,
  },
  SetBookmarks {
    conversation: ConversationId,
    bookmarks: Vec<crate::model::Bookmark>,
  },
  AddMemberWithHistory {
    conversation: ConversationId,
    user: UserId,
    history: HistoryPolicy,
  },
  InviteToSpace {
    invite: SpaceInvite,
  },
  SetThreadFollowed {
    conversation: ConversationId,
    thread: ThreadId,
    is_followed: bool,
  },
  MarkThreadRead {
    conversation: ConversationId,
    thread: ThreadId,
  },
  SaveSearch {
    name: String,
    query: String,
  },
  DeleteSavedSearch {
    id: u64,
  },
}

impl ChatAction {
  /// Runs the action on a backend. The original actions are left to the host, which handles
  /// some of them itself (enrollment state, admin flows); returns them back.
  pub fn apply_to(self, backend: &(impl ChatBackend + SearchBackend)) -> Option<Self> {
    match self {
      Self::PostMessage {
        conversation,
        maybe_thread,
        body,
      } => backend.post_message(conversation, maybe_thread, &body),
      Self::ToggleReaction {
        conversation,
        message,
        emoji,
      } => backend.toggle_reaction(conversation, message, &emoji),
      Self::MarkRead { conversation } => backend.mark_read(conversation),
      Self::SetFavorite {
        conversation,
        is_favorite,
      } => backend.set_favorite(conversation, is_favorite),
      Self::SetPresence {
        presence,
        maybe_until,
      } => backend.set_presence_until(presence, maybe_until),
      Self::SetCustomStatus { maybe_status } => backend.set_custom_status(maybe_status),
      Self::MarkActivityRead => backend.mark_activity_read(),
      Self::SetLastSeenVisibility { visibility } => backend.set_last_seen_visibility(visibility),
      Self::RefreshCallHistory => backend.refresh_call_history(),
      Self::RefreshOwnDevices => backend.refresh_own_devices(),
      Self::RevokeOwnDevice { device } => backend.revoke_own_device(device),
      Self::SetThreadFollowed {
        conversation,
        thread,
        is_followed,
      } => backend.set_thread_followed(conversation, thread, is_followed),
      Self::MarkThreadRead { conversation, thread } => backend.mark_thread_read(conversation, thread),
      Self::SendMessage { message } => backend.send_message(message),
      Self::EditMessage {
        conversation,
        message,
        body,
      } => backend.edit_message(conversation, message, &body),
      Self::DeleteMessage {
        conversation,
        message,
      } => backend.delete_message(conversation, message),
      Self::ForwardMessage {
        from,
        message,
        to,
        comment,
      } => backend.forward_message(from, message, to, &comment),
      Self::SetPinned {
        conversation,
        message,
        is_pinned,
      } => backend.set_pinned(conversation, message, is_pinned),
      Self::SetSaved {
        conversation,
        message,
        is_saved,
      } => backend.set_saved(conversation, message, is_saved),
      Self::AddBackgroundImage { name, picture } => backend.add_background_image(&name, picture.into_vec()),
      Self::DeleteBackgroundImage { id } => backend.delete_background_image(id),
      Self::AddOrgBackground { name, picture } => backend.add_org_background(&name, picture.into_vec()),
      Self::DeleteOrgBackground { id } => backend.delete_org_background(id),
      Self::SetOrgBackgroundRule { rule } => backend.set_org_background_rule(rule),
      Self::MarkUnread { conversation, from } => backend.mark_unread(conversation, from),
      Self::SetDraft {
        conversation,
        maybe_thread,
        body,
      } => backend.set_draft(conversation, maybe_thread, &body),
      Self::Acknowledge {
        conversation,
        message,
      } => backend.acknowledge(conversation, message),
      Self::RetrySend {
        conversation,
        message,
      } => backend.retry_send(conversation, message),
      Self::Typing { conversation } => backend.notify_typing(conversation),
      Self::DiscardUnsent {
        conversation,
        message,
      } => backend.discard_unsent(conversation, message),
      Self::DownloadFile {
        conversation,
        message,
      } => backend.download_file(conversation, message),
      Self::DownloadAttachment {
        conversation,
        message,
        file_id,
      } => backend.download_attachment(conversation, message, &file_id),
      Self::CreateShareLink {
        conversation,
        message,
        options,
      } => backend.create_share_link(conversation, message, options),
      Self::CreateSpace { space } => backend.create_space(space),
      Self::UpdateSpace { space, changes } => backend.update_space(space, changes),
      Self::CreateChannelWith { channel } => backend.create_channel_with(channel),
      Self::UpdateChannel {
        conversation,
        changes,
      } => backend.update_channel(conversation, changes),
      Self::JoinChannel { conversation } => backend.join_channel(conversation),
      Self::JoinSpace { space } => backend.join_space(space),
      Self::LeaveChannel { conversation } => backend.leave_channel(conversation),
      Self::CreateCategory { space, name } => backend.create_category(space, &name),
      Self::RenameCategory {
        space,
        category,
        name,
      } => backend.rename_category(space, category, &name),
      Self::DeleteCategory { space, category } => backend.delete_category(space, category),
      Self::AddMemberWithHistory {
        conversation,
        user,
        history,
      } => backend.add_member_with_history(conversation, user, history),
      Self::RemoveMember { conversation, user } => backend.remove_member(conversation, user),
      Self::RetryRemoval { conversation, user } => backend.retry_removal(conversation, user),
      Self::DiscardRemoval { conversation, user } => backend.discard_removal(conversation, user),
      Self::SetBookmarks {
        conversation,
        bookmarks,
      } => backend.set_bookmarks(conversation, bookmarks),
      Self::InviteToSpace { invite } => backend.invite_to_space(invite),
      Self::SaveSearch { name, query } => backend.save_search(&name, &query),
      Self::DeleteSavedSearch { id } => backend.delete_saved_search(id),
      original => return Some(original),
    }
    None
  }
}

/// Reads the UI asks a core outside it for, beyond [`ChatSnapshot`] and [`MessageWindow`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChatQuery {
  MessageExtras {
    conversation: ConversationId,
    messages: Vec<MessageId>,
  },
  PinnedMessages {
    conversation: ConversationId,
  },
  SavedItems,
  BackgroundImages,
  OrgBackgrounds,
  Permalink {
    conversation: ConversationId,
    message: MessageId,
  },
  ResolvePermalink {
    link: String,
  },
  FirstUnread {
    conversation: ConversationId,
  },
  IsWaitingForDevice {
    conversation: ConversationId,
  },
  MembershipIssues {
    conversation: ConversationId,
  },
  UnconfirmedRemovals,
  Draft {
    conversation: ConversationId,
    #[serde(default)]
    maybe_thread: Option<ThreadId>,
  },
  ConversationFiles {
    conversation: ConversationId,
  },
  ConversationImages {
    conversation: ConversationId,
    around: MessageId,
    max_messages: usize,
  },
  ShareLinks {
    conversation: ConversationId,
    message: MessageId,
  },
  MaxUploadBytes,
  BrowseChannels {
    space: SpaceId,
  },
  JoinableSpaces,
  Categories {
    space: SpaceId,
  },
  /// A raw query in the search box syntax (`from:`, `in:`, ...).
  Search {
    query: String,
  },
  RecentMentions,
  SavedSearches,
  Thread {
    conversation: ConversationId,
    thread: ThreadId,
  },
  FollowedThreads,
  IsThreadFollowed {
    conversation: ConversationId,
    thread: ThreadId,
  },
  SeenBy {
    conversation: ConversationId,
    message: MessageId,
  },
  TypingUsers {
    conversation: ConversationId,
  },
  HasExternalPeople {
    conversation: ConversationId,
  },
  Activity,
  LastSeen {
    user: UserId,
  },
  Privacy,
  CallHistory,
  OwnDevices,
  /// People by name, the server's directory included ([`ChatBackend::search_people`]).
  SearchPeople {
    query: String,
  },
  #[serde(other)]
  Unknown,
}

/// The answer to a [`ChatQuery`], same variant names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum ChatAnswer {
  MessageExtras {
    extras: Vec<MessageExtras>,
  },
  Messages {
    messages: Vec<Message>,
  },
  SavedItems {
    items: Vec<SavedItem>,
  },
  BackgroundImages {
    images: Vec<BackgroundImageInfo>,
  },
  OrgBackgrounds {
    backgrounds: OrgBackgrounds,
  },
  Link {
    maybe_link: Option<String>,
  },
  PermalinkTarget {
    maybe_target: Option<PermalinkTarget>,
  },
  Message {
    maybe_message: Option<MessageId>,
  },
  Draft {
    maybe_body: Option<String>,
  },
  Files {
    files: Vec<FileInfo>,
  },
  ShareLinks {
    links: Vec<ShareLink>,
  },
  Bytes {
    maybe_bytes: Option<u64>,
  },
  Channels {
    channels: Vec<BrowsableChannel>,
  },
  JoinableSpaces {
    spaces: Vec<crate::model::JoinableSpace>,
  },
  Categories {
    categories: Vec<CategoryInfo>,
  },
  SearchResults {
    results: SearchResults,
  },
  SearchHits {
    hits: Vec<SearchHit>,
  },
  SavedSearches {
    searches: Vec<SavedSearch>,
  },
  Flag {
    value: bool,
  },
  Threads {
    threads: Vec<ThreadSummary>,
  },
  Users {
    users: Vec<UserId>,
  },
  People {
    people: Vec<User>,
  },
  Activity {
    feed: ActivityFeed,
  },
  LastSeen {
    maybe_at: Option<TimestampMs>,
  },
  Privacy {
    settings: PrivacySettings,
  },
  CallHistory {
    calls: Vec<CallHistoryItem>,
  },
  Devices {
    devices: Vec<DeviceInfo>,
  },
  MembershipIssues {
    issues: Vec<MembershipIssue>,
  },
  UnconfirmedRemovals {
    removals: Vec<(ConversationId, UserId)>,
  },
  Unsupported,
}

impl ChatQuery {
  /// Answers the query from a backend (the host side of the IPC).
  pub fn answer(self, backend: &(impl ChatBackend + SearchBackend)) -> ChatAnswer {
    match self {
      Self::MessageExtras {
        conversation,
        messages,
      } => ChatAnswer::MessageExtras {
        extras: backend.message_extras(conversation, &messages),
      },
      Self::PinnedMessages { conversation } => ChatAnswer::Messages {
        messages: backend.pinned_messages(conversation),
      },
      Self::SavedItems => ChatAnswer::SavedItems {
        items: backend.saved_items(),
      },
      Self::BackgroundImages => ChatAnswer::BackgroundImages {
        images: backend.background_images(),
      },
      Self::OrgBackgrounds => ChatAnswer::OrgBackgrounds {
        backgrounds: backend.org_backgrounds(),
      },
      Self::Permalink {
        conversation,
        message,
      } => ChatAnswer::Link {
        maybe_link: backend.permalink(conversation, message),
      },
      Self::ResolvePermalink { link } => ChatAnswer::PermalinkTarget {
        maybe_target: backend.resolve_permalink(&link),
      },
      Self::IsWaitingForDevice { conversation } => ChatAnswer::Flag {
        value: backend.is_waiting_for_device(conversation),
      },
      Self::MembershipIssues { conversation } => ChatAnswer::MembershipIssues {
        issues: backend.membership_issues(conversation),
      },
      Self::UnconfirmedRemovals => ChatAnswer::UnconfirmedRemovals {
        removals: backend.unconfirmed_removals(),
      },
      Self::FirstUnread { conversation } => ChatAnswer::Message {
        maybe_message: backend.first_unread(conversation),
      },
      Self::Draft {
        conversation,
        maybe_thread,
      } => ChatAnswer::Draft {
        maybe_body: backend.draft(conversation, maybe_thread),
      },
      Self::ConversationFiles { conversation } => ChatAnswer::Files {
        files: backend.conversation_files(conversation),
      },
      Self::ConversationImages {
        conversation,
        around,
        max_messages,
      } => ChatAnswer::Files {
        files: backend.conversation_images(conversation, around, max_messages),
      },
      Self::ShareLinks {
        conversation,
        message,
      } => ChatAnswer::ShareLinks {
        links: backend.share_links(conversation, message),
      },
      Self::MaxUploadBytes => ChatAnswer::Bytes {
        maybe_bytes: backend.max_upload_bytes(),
      },
      Self::BrowseChannels { space } => ChatAnswer::Channels {
        channels: backend.browse_channels(space),
      },
      Self::JoinableSpaces => ChatAnswer::JoinableSpaces {
        spaces: backend.joinable_spaces(),
      },
      Self::Categories { space } => ChatAnswer::Categories {
        categories: backend.categories(space),
      },
      Self::Search { query } => ChatAnswer::SearchResults {
        results: backend.search(&crate::search::parse_query(&query)),
      },
      Self::RecentMentions => ChatAnswer::SearchHits {
        hits: backend.recent_mentions(),
      },
      Self::SavedSearches => ChatAnswer::SavedSearches {
        searches: backend.saved_searches(),
      },
      Self::HasExternalPeople { conversation } => ChatAnswer::Flag {
        value: backend.has_external_people(conversation),
      },
      Self::Thread { conversation, thread } => ChatAnswer::Messages {
        messages: backend.thread(conversation, thread),
      },
      Self::FollowedThreads => ChatAnswer::Threads {
        threads: backend.followed_threads(),
      },
      Self::IsThreadFollowed { conversation, thread } => ChatAnswer::Flag {
        value: backend.is_thread_followed(conversation, thread),
      },
      Self::TypingUsers { conversation } => ChatAnswer::Users {
        users: backend.typing_users(conversation),
      },
      Self::SeenBy {
        conversation,
        message,
      } => ChatAnswer::Users {
        users: backend.seen_by(conversation, message),
      },
      Self::Activity => ChatAnswer::Activity {
        feed: backend.activity(),
      },
      Self::LastSeen { user } => ChatAnswer::LastSeen {
        maybe_at: backend.last_seen(user),
      },
      Self::Privacy => ChatAnswer::Privacy {
        settings: backend.privacy(),
      },
      Self::CallHistory => ChatAnswer::CallHistory {
        calls: backend.call_history(),
      },
      Self::OwnDevices => ChatAnswer::Devices {
        devices: backend.own_devices(),
      },
      Self::SearchPeople { query } => ChatAnswer::People {
        people: backend.search_people(&query),
      },
      Self::Unknown => ChatAnswer::Unsupported,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn actions_round_trip_as_tagged_json() {
    let action = ChatAction::PostMessage {
      conversation: id_from_raw(7),
      maybe_thread: None,
      body: "hi".into(),
    };
    let json = serde_json::to_string(&action).expect("serialize");
    assert_eq!(
      json,
      r#"{"action":"post_message","conversation":"7","maybe_thread":null,"body":"hi"}"#
    );
    assert_eq!(serde_json::from_str::<ChatAction>(&json).expect("parse"), action);
  }

  #[test]
  fn group_dm_action_lists_the_people() {
    let action: ChatAction =
      serde_json::from_str(r#"{"action":"open_group_dm","users":["2","3"]}"#).expect("parse");
    assert_eq!(
      action,
      ChatAction::OpenGroupDm {
        users: vec![id_from_raw(2), id_from_raw(3)]
      }
    );
  }

  #[test]
  fn typing_action_and_query_are_tagged() {
    let typing: ChatAction =
      serde_json::from_str(r#"{"action":"typing","conversation":"7"}"#).expect("parse");
    assert_eq!(
      typing,
      ChatAction::Typing {
        conversation: id_from_raw(7)
      }
    );
    let query: ChatQuery =
      serde_json::from_str(r#"{"kind":"typing_users","conversation":"7"}"#).expect("parse");
    assert_eq!(
      query,
      ChatQuery::TypingUsers {
        conversation: id_from_raw(7)
      }
    );
  }

  #[test]
  fn failed_send_actions_are_tagged() {
    let retry: ChatAction =
      serde_json::from_str(r#"{"action":"retry_send","conversation":"7","message":"9"}"#).expect("parse");
    assert_eq!(
      retry,
      ChatAction::RetrySend {
        conversation: id_from_raw(7),
        message: id_from_raw(9),
      }
    );
    let discard: ChatAction =
      serde_json::from_str(r#"{"action":"discard_unsent","conversation":"7","message":"9"}"#).expect("parse");
    assert_eq!(
      discard,
      ChatAction::DiscardUnsent {
        conversation: id_from_raw(7),
        message: id_from_raw(9),
      }
    );
    // Extras from an older core have no send state: not failed.
    let extras: crate::actions::MessageExtras = serde_json::from_str(r#"{"message":"9"}"#).expect("parse");
    assert!(!extras.is_send_failed);
  }

  #[test]
  fn thread_actions_and_queries_are_tagged() {
    let follow: ChatAction = serde_json::from_str(
      r#"{"action":"set_thread_followed","conversation":"7","thread":"9","is_followed":true}"#,
    )
    .expect("parse");
    assert_eq!(
      follow,
      ChatAction::SetThreadFollowed {
        conversation: id_from_raw(7),
        thread: id_from_raw(9),
        is_followed: true,
      }
    );
    let query: ChatQuery =
      serde_json::from_str(r#"{"kind":"thread","conversation":"7","thread":"9"}"#).expect("parse");
    assert_eq!(
      query,
      ChatQuery::Thread {
        conversation: id_from_raw(7),
        thread: id_from_raw(9),
      }
    );
    assert_eq!(
      serde_json::to_string(&ChatAnswer::Users { users: Vec::new() }).expect("serialize"),
      r#"{"answer":"users","users":[]}"#
    );
  }

  #[test]
  fn call_actions_are_tagged_and_unknown_ones_tolerated() {
    let action = CallAction::SetMuted { is_muted: true };
    let json = serde_json::to_string(&action).expect("serialize");
    assert_eq!(json, r#"{"action":"set_muted","is_muted":true}"#);
    assert_eq!(serde_json::from_str::<CallAction>(&json).expect("parse"), action);
    assert_eq!(
      serde_json::from_str::<CallAction>(r#"{"action":"raise_hand"}"#).expect("parse"),
      CallAction::Unknown
    );
    let join = CallAction::Join {
      request: CallRequest {
        conversation: id_from_raw(5),
        kind: crate::calls::CallKind::Direct,
      },
    };
    assert_eq!(
      serde_json::to_string(&join).expect("serialize"),
      r#"{"action":"join","request":{"conversation":"5","kind":"direct"}}"#
    );
  }

  #[test]
  fn video_actions_round_trip() {
    use crate::calls::BackgroundPreset;

    for action in [
      CallAction::SetCamera { is_on: true },
      CallAction::SetBackgroundEffect {
        effect: BackgroundEffect::Preset(BackgroundPreset::Office),
      },
      CallAction::StartScreenShare { has_sound: true },
      CallAction::OpenSystemVideoEffects,
    ] {
      let json = serde_json::to_string(&action).expect("serialize");
      assert_eq!(serde_json::from_str::<CallAction>(&json).expect("parse"), action);
    }
    assert_eq!(
      serde_json::to_string(&CallAction::SetBackgroundEffect {
        effect: BackgroundEffect::BlurLight
      })
      .expect("serialize"),
      r#"{"action":"set_background_effect","effect":"blur_light"}"#
    );
  }

  #[test]
  fn screen_share_without_a_sound_flag_shares_the_picture_only() {
    let older: CallAction = serde_json::from_str(r#"{"action":"start_screen_share"}"#).expect("parse");
    assert_eq!(older, CallAction::StartScreenShare { has_sound: false });
  }

  #[test]
  fn status_is_tagged_by_state() {
    let status = CoreStatus::NeedsEnrollment {
      has_existing_identity: true,
      was_data_lost: false,
    };
    assert_eq!(
      serde_json::to_string(&status).expect("serialize"),
      r#"{"state":"needs_enrollment","has_existing_identity":true}"#
    );
    let lost: CoreStatus = serde_json::from_str(
      r#"{"state":"needs_enrollment","has_existing_identity":true,"was_data_lost":true}"#,
    )
    .expect("parse");
    assert_eq!(
      lost,
      CoreStatus::NeedsEnrollment {
        has_existing_identity: true,
        was_data_lost: true
      }
    );
  }

  #[test]
  fn store_health_is_forward_compatible() {
    assert_eq!(
      serde_json::to_string(&StoreHealth::QuotaExceeded).expect("serialize"),
      r#""quota_exceeded""#
    );
    let newer: StoreHealth = serde_json::from_str(r#""read_only""#).expect("parse");
    assert_eq!(newer, StoreHealth::Unknown);
  }
}
