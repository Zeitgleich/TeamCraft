//! Wire types between a client core running outside the UI (the desktop shell's Rust side) and the
//! UI in the webview. The core owns the encrypted store and the network; the UI keeps a mirror of
//! what it shows and asks for more through these messages.

use serde::{Deserialize, Serialize};

use teamcraft_proto::{
  BackgroundId, Base64Bytes, CallRosterEntry, CategoryId, HistoryPolicy, OrgBackgroundId, SignInReason,
};

use crate::actions::{
  BrowsableChannel, CategoryInfo, ChannelChanges, FileInfo, LinkedMessage, MediaAnchor, MediaPage,
  MessageExtras, NewChannel, NewSpace, OutgoingMessage, PermalinkTarget, SavedItem, ShareLink,
  ShareLinkOptions, SpaceChanges, SpaceInvite,
};
use crate::activity::ActivityFeed;
use crate::admin::DeviceInfo;
use crate::backend::{ChatBackend, LoadingList, SearchBackend};
use crate::calls::{
  ActiveCall, BackgroundEffect, BackgroundImageInfo, CallEndReason, CallHistoryItem, CallMoveOffer,
  CallPreview, CallRequest, CallStatusSounds, CameraFallback, IncomingCall, JoinMedia, MediaDevices,
  OrgBackgroundRule, OrgBackgrounds, PreviewCamera, PushToTalk, RemoteVideoTrack, RunningCall,
  ScreenContentMode, ScreenShareResolution, VideoQuality,
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

/// Event name the host emits with a failure no UI request waited for (a background command of a
/// read); the payload is `failure::ActionFailure` wire text.
pub const CORE_FAILED_EVENT: &str = "core-failed";

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
  /// The pre-join check's camera (#442).
  #[serde(default)]
  pub preview_camera: PreviewCamera,
  /// The camera that stood in when the chosen one didn't open, at the last camera start (#819).
  #[serde(default)]
  pub maybe_camera_fallback: Option<CameraFallback>,
  /// This device's roster entry, once it joined a call (issue #976): which of a running call's
  /// devices are the user's others.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub maybe_this_device: Option<CallRosterEntry>,
}

/// Something the user did in or about a call.
///
/// The desktop shell (`call_action`) and the browser's call module (`act`) carry it out. A failed
/// action answers with `ActionFailure` wire text. One that went through answers with nothing, or
/// with the wire text of a part of it that failed without undoing it ([`CallActionAnswer`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CallAction {
  /// Joins with the pre-join screen's choice ([`JoinMedia`]).
  Join {
    request: CallRequest,
    /// Join with the microphone muted from the first frame.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    is_muted: bool,
    /// Start the camera with the call (#453). Older UIs send no flag and turn the camera on with
    /// [`CallAction::SetCamera`] after the join.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    is_camera_on: bool,
    /// The video codecs the page decodes (#565), for the desktop shell, whose page shows the
    /// call's video: it announces them to the call. Empty (older UIs, the browser's own call
    /// module, which asks its browser itself): H.264 only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    video_decoders: Vec<teamcraft_proto::VideoCodec>,
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
  /// Takes the call over from the account's other devices, which leave it (#505).
  MoveHere,
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
    /// What the share keeps under pressure (older UIs send none: detail).
    #[serde(default)]
    content_mode: ScreenContentMode,
    /// How large the share may be (#565; older UIs send none: standard). The UI asks for 4K only
    /// where the workspace allows it.
    #[serde(default)]
    resolution: ScreenShareResolution,
  },
  StopScreenShare,
  /// Opens the system's video effects panel (macOS: Portrait, background, Studio Light).
  OpenSystemVideoEffects,
  /// Which call status sounds play (a device setting; also before any call).
  SetStatusSounds {
    sounds: CallStatusSounds,
  },
  /// The highest quality the own camera is sent at (#967; a device setting, also before any
  /// call). A running camera restarts with it.
  SetCameraQuality {
    quality: VideoQuality,
  },
  /// How sharp another device's track comes to this device (#967): Auto by the size its tile
  /// shows it at, else at most the lines picked. Kept while the track is published.
  SetVideoQuality {
    stream_id: teamcraft_proto::MediaStreamId,
    quality: VideoQuality,
  },
  /// Lists the cameras, microphones and speakers again (the pre-join screen opened).
  RefreshDevices,
  /// Starts, changes or ends the pre-join check of the camera and microphone (#442). Refused
  /// while a call runs on this device: the call has them.
  SetPreview {
    preview: CallPreview,
  },
  /// Plays a short test sound on the speakers (`None`: the system default); finishes when it
  /// has played, fails when it can't play. The desktop shell's: the browser page plays it itself,
  /// since it has to start inside the click.
  TestSpeakers {
    #[serde(default)]
    maybe_device: Option<String>,
  },
  #[serde(other)]
  Unknown,
}

/// What a [`CallAction`] that went through answers: `None`, or a part of it that failed without
/// undoing it, as `ActionFailure` wire text. A join whose camera didn't start (#453) is still a
/// join: the call runs, and the UI says why the camera stayed off.
pub type CallActionAnswer = Option<String>;

impl CallAction {
  /// Joins `request` with the pre-join choice.
  pub fn join(request: CallRequest, media: JoinMedia) -> Self {
    Self::Join {
      request,
      is_muted: media.is_muted,
      is_camera_on: media.is_camera_on,
      video_decoders: Vec::new(),
    }
  }
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
  /// Where each conversation sits in the lists above (derived, never sent).
  #[serde(skip)]
  pub index: crate::snapshot_changes::ConversationIndex,
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
  /// Which statuses the verified policy allows (custom ones, presets); `None` (not known yet, or an
  /// older shell) allows them, as the server's default does. The server enforces it either way.
  #[serde(rename = "presence_policy", default, skip_serializing_if = "Option::is_none")]
  pub maybe_presence_policy: Option<teamcraft_proto::PresencePolicy>,
  /// How large screen shares may be under the verified policy (#565); `None` (not known yet, or
  /// an older shell): the default, 4K offered.
  #[serde(
    rename = "screen_share_policy",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_screen_share_policy: Option<teamcraft_proto::ScreenSharePolicy>,
  /// The notetaker group of the verified policy (e2ee.md §8.2); `None` before a policy was
  /// verified (or from an older shell), which shows no notetaker controls.
  #[serde(rename = "notetaker", default, skip_serializing_if = "Option::is_none")]
  pub maybe_notetaker: Option<teamcraft_proto::NotetakerPolicy>,
  /// The key backup as this device knows it (issue #410); `None` from an older shell.
  #[serde(rename = "backup", default, skip_serializing_if = "Option::is_none")]
  pub maybe_backup: Option<crate::security::KeyBackupState>,
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
      index: crate::snapshot_changes::ConversationIndex::default(),
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
    let (total, messages) = backend.message_page(conversation, start, len);
    Self {
      conversation,
      start,
      total,
      messages,
      undecrypted: backend.undecrypted_count(conversation),
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
    /// When the relay closes and the code stops working, on this machine's clock (issue #998).
    /// `None` until joined, or from a server that doesn't say.
    #[serde(default)]
    maybe_expires_at_ms: Option<i64>,
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

/// A signed-in device showing a code for a new phone to scan (*Link a phone*, e2ee.md §5, issue
/// #998). Once the phone scanned it, the user types the code the phone shows, as for a code this
/// device scanned.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PhoneLinkStatus {
  #[default]
  Idle,
  WaitingForScan {
    code: String,
    /// When the code stops working, on this machine's clock; `None` from a server that doesn't say.
    maybe_expires_at_ms: Option<i64>,
  },
  /// The phone scanned the code and ran the handshake: type the code it shows.
  Scanned,
  Failed {
    message: String,
  },
}

/// A new device of the user asks to be linked; every signed-in device of the account shows it as a
/// prompt (e2ee.md §5). The handshake key stays in the core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRequestView {
  pub id: String,
  pub device_name: String,
  pub platform: teamcraft_proto::DeviceLinkPlatform,
  /// Seconds the request stays open, when it was read.
  pub seconds_left: u64,
}

/// A slice of one conversation's feed, oldest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageWindow {
  pub conversation: ConversationId,
  pub start: usize,
  /// Length of the whole feed at the time of reading.
  pub total: usize,
  pub messages: Vec<Message>,
  /// Messages of the conversation this device received but can't read yet (keys not arrived):
  /// not in the feed, so a conversation of only such messages would look empty without it.
  #[serde(default)]
  pub undecrypted: usize,
}

/// The newest messages of one conversation's feed down to the oldest one a paging list holds,
/// and the page before it, oldest first (issue #1058).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MessageTail {
  pub conversation: ConversationId,
  pub messages: Vec<Message>,
  /// Older messages exist: the list can page further back.
  pub has_older: bool,
  /// As [`MessageWindow::undecrypted`].
  #[serde(default)]
  pub undecrypted: usize,
}

/// Where the local client stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CoreStatus {
  /// No signed-in account yet.
  NotStarted,
  /// First run on this device: keys have to be created or recovered.
  NeedsEnrollment {
    /// The account already has keys on the server, so the recovery kit (or a legacy passphrase)
    /// unlocks them.
    has_existing_identity: bool,
    /// This device was set up before, but its local data is gone (the browser deleted it): the user
    /// is told why setup is asked for again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    was_data_lost: bool,
    /// The desktop app kept this account's backup key in the OS keychain (e2ee.md §6.5.6), so the
    /// account can be restored on this computer without the recovery kit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    can_restore_on_device: bool,
  },
  Ready {
    /// Fingerprint of the account's identity key.
    identity_fingerprint: String,
  },
  /// The encrypted data could not be opened (keystore, database); `message` is for details.
  Failed { message: String },
  /// The local database can't be read by this version, and a retry won't change that (issue
  /// #160): the user can set the device up again, which sets the file aside instead of deleting it.
  StoreUnreadable { problem: StoreProblem },
  /// The client core ended unexpectedly (an internal error such as a panic), not because of the
  /// keystore or the database: the user is asked to restart the app and report it.
  Stopped,
}

/// Why the local database can't be read ([`CoreStatus::StoreUnreadable`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreProblem {
  /// The file is damaged, or the key that opens it is gone.
  Damaged,
  /// A newer version of the app changed it: update the app.
  NewerVersion,
  #[serde(other)]
  Unknown,
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
    /// What answered instead of the server, if the core found out.
    #[serde(rename = "interception", default, skip_serializing_if = "Option::is_none")]
    maybe_interception: Option<NetworkInterception>,
  },
  /// The server refused the access token: the user has to sign in again, for `reason`.
  SignInRequired {
    #[serde(default, skip_serializing_if = "SignInReason::is_unknown")]
    reason: SignInReason,
  },
}

/// Something on the network that answered instead of the server (issue #157).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkInterception {
  /// The network's login page (hotel, train, guest Wi-Fi).
  CaptivePortal,
  /// A certificate the device doesn't trust, usually a proxy that inspects encrypted traffic.
  UntrustedCertificate,
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
  /// Wraps the key backup of a new account; recovers an existing identity when the account has one.
  pub secret: EnrollSecret,
  /// Shown in the user's device list.
  pub device_name: String,
}

/// What sets this device up (e2ee.md §6.5). `Debug` never shows a key or passphrase.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnrollSecret {
  /// The recovery key of a recovery kit: the new kit's at first sign-in, the saved one when
  /// recovering.
  RecoveryKey { recovery_key: String },
  /// A legacy recovery passphrase (recovering an account that still has one).
  Passphrase { passphrase: String },
  /// Desktop: the backup key this computer kept in its OS keychain (e2ee.md §6.5.6).
  KeptOnDevice,
  /// The device of a meeting guest (browser only): an ephemeral identity with no backup.
  Guest,
}

impl std::fmt::Debug for EnrollSecret {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(match self {
      Self::RecoveryKey { .. } => "RecoveryKey([redacted])",
      Self::Passphrase { .. } => "Passphrase([redacted])",
      Self::KeptOnDevice => "KeptOnDevice",
      Self::Guest => "Guest",
    })
  }
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
  MarkReadThrough {
    conversation: ConversationId,
    message: MessageId,
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
  /// Read again how far the other members read (a DM was opened, or the connection came back
  /// while it is open): receipts that arrived while offline were missed.
  RefreshReadReceipts {
    conversation: ConversationId,
  },
  /// Sign out one of this user's other devices.
  RevokeOwnDevice {
    device: DeviceId,
  },
  /// Rename one of this user's devices (issue #409).
  RenameOwnDevice {
    device: DeviceId,
    name: String,
  },
  /// Ask the server to send a test push to one of this user's devices; the outcome shows in the
  /// device list.
  TestPushOwnDevice {
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
  /// Wraps the key backup with the recovery key of a new recovery kit the user saved; the previous
  /// kit and a legacy passphrase stop working for recovery (e2ee.md §6.5.7).
  SetRecoveryKey {
    recovery_key: String,
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
  /// An admin asks the owners of a private channel to let them in (admin.md §2.4).
  RequestAdminJoin {
    conversation: ConversationId,
  },
  WithdrawAdminJoin {
    conversation: ConversationId,
  },
  /// A channel owner lets an admin in who asked, sharing `history` (older UIs send none: from the
  /// join on, what a self-join gets).
  ApproveAdminJoin {
    conversation: ConversationId,
    user: UserId,
    #[serde(default = "history_from_join")]
    history: HistoryPolicy,
  },
  DeclineAdminJoin {
    conversation: ConversationId,
    user: UserId,
  },
  /// An admin removes a deactivated user from all chats: signed removals per channel, then the
  /// server's step (admin.md §3.2.1).
  AdminRemoveFromAllChats {
    user: UserId,
  },
  /// An admin deletes an account: signed removals per channel, then `DELETE /admin/users/{id}`
  /// (admin.md §3.2.1).
  AdminDeleteUser {
    user: UserId,
  },
  /// An admin removes a user left over in the encryption group of a conversation this device is in
  /// (issue #734).
  AdminRemoveGroupLeftover {
    conversation: ConversationId,
    user: UserId,
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
  /// The user is typing (send while the text changes; the core throttles to one per few seconds),
  /// with `maybe_thread` a reply in that thread.
  Typing {
    conversation: ConversationId,
    #[serde(default)]
    maybe_thread: Option<ThreadId>,
  },
  DiscardUnsent {
    conversation: ConversationId,
    message: MessageId,
  },
  CancelUpload {
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
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
    file_id: String,
    options: ShareLinkOptions,
  },
  RevokeShareLink {
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
    share_id: String,
  },
  RefreshShareLinks {
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
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
  RetryDmSetup {
    conversation: ConversationId,
  },
  SetUpMeetingChat {
    conversation: ConversationId,
    invitees: Vec<UserId>,
  },
  UpdateMeetingChatMembers {
    conversation: ConversationId,
    invitees: Vec<UserId>,
    removed: Vec<UserId>,
  },
  AddPeopleToCall {
    from: ConversationId,
    in_call: Vec<UserId>,
    added: Vec<UserId>,
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    maybe_title: Option<String>,
    key: teamcraft_proto::IdempotencyKey,
  },
  AcceptCallMove {
    from: ConversationId,
    to: ConversationId,
  },
  LetIntoMeetingChat {
    conversation: ConversationId,
    user: UserId,
  },
  PrepareGuestLink {
    conversation: ConversationId,
    meeting: teamcraft_proto::MeetingId,
  },
  LetGuestIntoMeetingChat {
    conversation: ConversationId,
    meeting: teamcraft_proto::MeetingId,
    guest: UserId,
    display_name: String,
    #[serde(rename = "proof", default, skip_serializing_if = "Option::is_none")]
    maybe_proof: Option<Base64Bytes>,
    access_ends_at_ms: i64,
  },
  SetBookmarks {
    conversation: ConversationId,
    bookmarks: Vec<crate::model::Bookmark>,
  },
  AddIntegrationSource {
    conversation: ConversationId,
    source: crate::model::NewIntegrationSource,
  },
  RevokeIntegrationSource {
    conversation: ConversationId,
    source: teamcraft_proto::SourceId,
  },
  RequestNotetaker {
    conversation: ConversationId,
    request: teamcraft_proto::NotetakerRequest,
  },
  AddMemberWithHistory {
    conversation: ConversationId,
    user: UserId,
    history: HistoryPolicy,
  },
  InviteToSpace {
    invite: SpaceInvite,
  },
  AddSpaceMembers {
    space: SpaceId,
    users: Vec<UserId>,
    is_guest: bool,
  },
  RefreshSpaceRoster {
    space: SpaceId,
  },
  AddPeopleToChannel {
    conversation: ConversationId,
    people: Vec<(UserId, bool)>,
    history: HistoryPolicy,
    joins_space: bool,
  },
  SetSpaceRole {
    space: SpaceId,
    user: UserId,
    role: crate::model::SpaceRole,
  },
  SetChannelRole {
    conversation: ConversationId,
    user: UserId,
    role: crate::model::ChannelRole,
  },
  ConfirmChannelRole {
    conversation: ConversationId,
    user: UserId,
  },
  RefreshChannelRoles {
    conversation: ConversationId,
  },
  RemoveSpaceMember {
    space: SpaceId,
    user: UserId,
  },
  RevokeSpaceInvite {
    space: SpaceId,
    invite: crate::model::InviteId,
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

/// How soon the UI wants a file's bytes (`core_attachment`): the image viewer before the previews.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentPriority {
  /// A preview in the conversation: in turn.
  #[default]
  Background,
  /// The user opened it.
  Now,
}

/// An admin let in without an explicit choice gets what a self-join gets (e2ee.md §4.7).
fn history_from_join() -> HistoryPolicy {
  HistoryPolicy::FromJoin
}

impl ChatAction {
  /// A new message: the host answers it once it is in the outbox and sends it afterwards, so the
  /// message shows at once, also while a sync or an upload holds the client.
  pub fn is_new_message(&self) -> bool {
    matches!(self, Self::SendMessage { .. } | Self::PostMessage { .. })
  }

  /// Where a typing announcement is: its conversation, and the thread for a reply. It is passing
  /// and changes nothing stored or shown: the host sends it on the open gateway connection right
  /// away, tells the UI of no change, and drops it while offline or busy connecting (a late
  /// indicator would be wrong).
  pub fn typing_place(&self) -> Option<(ConversationId, Option<ThreadId>)> {
    match self {
      Self::Typing {
        conversation,
        maybe_thread,
      } => Some((*conversation, *maybe_thread)),
      _ => None,
    }
  }

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
      Self::MarkReadThrough {
        conversation,
        message,
      } => backend.mark_read_through(conversation, message),
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
      Self::RequestAdminJoin { conversation } => backend.request_admin_join(conversation),
      Self::WithdrawAdminJoin { conversation } => backend.withdraw_admin_join(conversation),
      Self::ApproveAdminJoin {
        conversation,
        user,
        history,
      } => backend.approve_admin_join(conversation, user, history),
      Self::DeclineAdminJoin { conversation, user } => backend.decline_admin_join(conversation, user),
      Self::SetLastSeenVisibility { visibility } => backend.set_last_seen_visibility(visibility),
      Self::RefreshCallHistory => backend.refresh_call_history(),
      Self::RefreshOwnDevices => backend.refresh_own_devices(),
      Self::RefreshReadReceipts { conversation } => backend.refresh_read_receipts(conversation),
      Self::RevokeOwnDevice { device } => backend.revoke_own_device(device),
      Self::RenameOwnDevice { device, name } => backend.rename_own_device(device, name),
      Self::TestPushOwnDevice { device } => backend.test_push_own_device(device),
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
      Self::Typing {
        conversation,
        maybe_thread,
      } => backend.notify_typing(conversation, maybe_thread),
      Self::DiscardUnsent {
        conversation,
        message,
      } => backend.discard_unsent(conversation, message),
      Self::CancelUpload {
        conversation,
        message,
        file_id,
      } => backend.cancel_upload(conversation, message, &file_id),
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
        file_id,
        options,
      } => backend.create_share_link(conversation, message, &file_id, options),
      Self::RevokeShareLink {
        conversation,
        message,
        file_id,
        share_id,
      } => backend.revoke_share_link(conversation, message, &file_id, &share_id),
      Self::RefreshShareLinks {
        conversation,
        message,
        file_id,
      } => backend.refresh_share_links(conversation, message, &file_id),
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
      Self::RetryDmSetup { conversation } => backend.retry_dm_setup(conversation),
      Self::SetUpMeetingChat {
        conversation,
        invitees,
      } => backend.set_up_meeting_chat(conversation, invitees),
      Self::UpdateMeetingChatMembers {
        conversation,
        invitees,
        removed,
      } => backend.update_meeting_chat_members(conversation, invitees, removed),
      Self::AddPeopleToCall {
        from,
        in_call,
        added,
        maybe_title,
        key,
      } => backend.add_people_to_call(from, in_call, added, maybe_title, key),
      Self::AcceptCallMove { from, to } => backend.accept_call_move(from, to),
      Self::LetIntoMeetingChat { conversation, user } => backend.let_into_meeting_chat(conversation, user),
      Self::PrepareGuestLink {
        conversation,
        meeting,
      } => backend.prepare_guest_link(conversation, meeting),
      Self::LetGuestIntoMeetingChat {
        conversation,
        meeting,
        guest,
        display_name,
        maybe_proof,
        access_ends_at_ms,
      } => backend.let_guest_into_meeting_chat(
        conversation,
        meeting,
        guest,
        display_name,
        maybe_proof.map(|proof| proof.as_slice().to_vec()),
        access_ends_at_ms,
      ),
      Self::SetBookmarks {
        conversation,
        bookmarks,
      } => backend.set_bookmarks(conversation, bookmarks),
      Self::AddIntegrationSource { conversation, source } => {
        backend.add_integration_source(conversation, source)
      },
      Self::RevokeIntegrationSource { conversation, source } => {
        backend.revoke_integration_source(conversation, source)
      },
      Self::RequestNotetaker {
        conversation,
        request,
      } => backend.request_notetaker(conversation, request),
      Self::InviteToSpace { invite } => backend.invite_to_space(invite),
      Self::AddSpaceMembers {
        space,
        users,
        is_guest,
      } => backend.add_space_members(space, users, is_guest),
      Self::RefreshSpaceRoster { space } => backend.refresh_space_roster(space),
      Self::AddPeopleToChannel {
        conversation,
        people,
        history,
        joins_space,
      } => backend.add_people_to_channel(conversation, people, history, joins_space),
      Self::SetSpaceRole { space, user, role } => backend.set_space_role(space, user, role),
      Self::SetChannelRole {
        conversation,
        user,
        role,
      } => backend.set_channel_role(conversation, user, role),
      Self::ConfirmChannelRole { conversation, user } => backend.confirm_channel_role(conversation, user),
      Self::RefreshChannelRoles { conversation } => backend.refresh_channel_roles(conversation),
      Self::RemoveSpaceMember { space, user } => backend.remove_space_member(space, user),
      Self::RevokeSpaceInvite { space, invite } => backend.revoke_space_invite(space, invite),
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
  LinkedMessage {
    link: String,
  },
  FirstUnread {
    conversation: ConversationId,
  },
  NewestMessage {
    conversation: ConversationId,
  },
  IsWaitingForDevice {
    conversation: ConversationId,
  },
  /// The `#fragment` values of a guest link to the meeting, once the core made them.
  GuestLinkFragment {
    meeting: teamcraft_proto::MeetingId,
  },
  MembershipIssues {
    conversation: ConversationId,
  },
  UnconfirmedRemovals,
  GroupLeftovers,
  Draft {
    conversation: ConversationId,
    #[serde(default)]
    maybe_thread: Option<ThreadId>,
  },
  ConversationFiles {
    conversation: ConversationId,
  },
  ConversationMedia {
    conversation: ConversationId,
    anchor: MediaAnchor,
    max_messages: usize,
  },
  ShareLinks {
    conversation: ConversationId,
    message: MessageId,
    file_id: String,
  },
  MaxUploadBytes,
  BrowseChannels {
    space: SpaceId,
  },
  JoinableSpaces,
  SpaceMembers {
    space: SpaceId,
  },
  ChannelRoles {
    conversation: ConversationId,
  },
  IntegrationSources {
    conversation: ConversationId,
  },
  CanManageIntegrationSources,
  CanSignChannelRoles {
    conversation: ConversationId,
  },
  SpaceInvites {
    space: SpaceId,
  },
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
  /// Everyone typing anywhere (#882), one answer for the whole chat list.
  TypingNow,
  HasExternalPeople {
    conversation: ConversationId,
  },
  CallMoveOffer {
    from: ConversationId,
    to: ConversationId,
  },
  Activity,
  LastSeen {
    user: UserId,
  },
  Privacy,
  CallHistory,
  /// Whether a list has had its first answer ([`ChatBackend::is_loaded`]).
  ListLoaded {
    list: LoadingList,
  },
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
  Typing {
    typing: Vec<crate::model::TypingNow>,
  },
  CallMoveOffer {
    maybe_offer: Option<CallMoveOffer>,
  },
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
  LinkedMessage {
    maybe_linked: Option<LinkedMessage>,
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
  MediaPage {
    page: MediaPage,
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
  SpaceMembers {
    #[serde(default)]
    roster: crate::model::SpaceRoster,
  },
  ChannelRoles {
    #[serde(default)]
    members: Vec<crate::model::ChannelMember>,
  },
  IntegrationSources {
    #[serde(default)]
    sources: Vec<crate::model::IntegrationSource>,
  },
  CanManageIntegrationSources {
    #[serde(default)]
    can_manage: bool,
  },
  CanSignChannelRoles {
    #[serde(default)]
    can_sign: bool,
  },
  SpaceInvites {
    invites: Vec<crate::model::PendingInvite>,
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
  ListLoaded {
    is_loaded: bool,
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
  GroupLeftovers {
    leftovers: Vec<(ConversationId, UserId)>,
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
      Self::LinkedMessage { link } => ChatAnswer::LinkedMessage {
        maybe_linked: backend.linked_message(&link),
      },
      Self::GuestLinkFragment { meeting } => ChatAnswer::Link {
        maybe_link: backend.guest_link_fragment(meeting),
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
      Self::GroupLeftovers => ChatAnswer::GroupLeftovers {
        leftovers: backend.group_leftovers(),
      },
      Self::FirstUnread { conversation } => ChatAnswer::Message {
        maybe_message: backend.first_unread(conversation),
      },
      Self::NewestMessage { conversation } => ChatAnswer::Message {
        maybe_message: backend.newest_message(conversation),
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
      Self::ConversationMedia {
        conversation,
        anchor,
        max_messages,
      } => ChatAnswer::MediaPage {
        // The core reads its own store and always answers.
        page: backend
          .conversation_media(conversation, anchor, max_messages)
          .unwrap_or_default(),
      },
      Self::ShareLinks {
        conversation,
        message,
        file_id,
      } => ChatAnswer::ShareLinks {
        links: backend.share_links(conversation, message, &file_id),
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
      Self::ChannelRoles { conversation } => ChatAnswer::ChannelRoles {
        members: backend.channel_roles(conversation),
      },
      Self::IntegrationSources { conversation } => ChatAnswer::IntegrationSources {
        sources: backend.integration_sources(conversation),
      },
      Self::CanManageIntegrationSources => ChatAnswer::CanManageIntegrationSources {
        can_manage: backend.can_manage_integration_sources(),
      },
      Self::CanSignChannelRoles { conversation } => ChatAnswer::CanSignChannelRoles {
        can_sign: backend.can_sign_channel_roles(conversation),
      },
      Self::SpaceMembers { space } => ChatAnswer::SpaceMembers {
        roster: backend.space_members(space),
      },
      Self::SpaceInvites { space } => ChatAnswer::SpaceInvites {
        invites: backend.space_invites(space),
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
      Self::CallMoveOffer { from, to } => ChatAnswer::CallMoveOffer {
        maybe_offer: backend.call_move_offer(from, to),
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
      Self::TypingNow => ChatAnswer::Typing {
        typing: backend.typing_now(),
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
      Self::ListLoaded { list } => ChatAnswer::ListLoaded {
        is_loaded: backend.is_loaded(list),
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
        conversation: id_from_raw(7),
        maybe_thread: None,
      }
    );
    // Hosts send it at once instead of running it as a command.
    assert_eq!(typing.typing_place(), Some((id_from_raw(7), None)));
    let in_thread: ChatAction =
      serde_json::from_str(r#"{"action":"typing","conversation":"7","maybe_thread":"9"}"#).expect("parse");
    assert_eq!(
      in_thread.typing_place(),
      Some((id_from_raw(7), Some(id_from_raw(9))))
    );
    let mark_read = ChatAction::MarkRead {
      conversation: id_from_raw(7),
    };
    assert_eq!(mark_read.typing_place(), None);
    let everywhere: ChatQuery = serde_json::from_str(r#"{"kind":"typing_now"}"#).expect("parse");
    assert_eq!(everywhere, ChatQuery::TypingNow);
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
  fn the_mobile_apps_ask_for_receipts_by_a_stable_name() {
    let refresh: ChatAction =
      serde_json::from_str(r#"{"action":"refresh_read_receipts","conversation":"7"}"#).expect("parse");
    assert_eq!(
      refresh,
      ChatAction::RefreshReadReceipts {
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
    let cancel: ChatAction = serde_json::from_str(
      r#"{"action":"cancel_upload","conversation":"7","message":"9","file_id":"pending-1"}"#,
    )
    .expect("parse");
    assert_eq!(
      cancel,
      ChatAction::CancelUpload {
        conversation: id_from_raw(7),
        message: id_from_raw(9),
        file_id: "pending-1".into(),
      }
    );
    // Extras from an older core have no send or read state: not failed, not read.
    let extras: crate::actions::MessageExtras = serde_json::from_str(r#"{"message":"9"}"#).expect("parse");
    assert!(!extras.is_send_failed);
    assert!(!extras.is_read);
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
  }

  /// #453: the pre-join choice travels with the join; a join without it (an older UI) starts
  /// unmuted with the camera off, as before.
  #[test]
  fn a_join_carries_the_pre_join_choice() {
    let request = CallRequest {
      conversation: id_from_raw(5),
      kind: crate::calls::CallKind::Direct,
    };
    let plain = CallAction::join(request, JoinMedia::default());
    assert_eq!(
      serde_json::to_string(&plain).expect("serialize"),
      r#"{"action":"join","request":{"conversation":"5","kind":"direct"}}"#
    );
    assert_eq!(
      serde_json::from_str::<CallAction>(
        r#"{"action":"join","request":{"conversation":"5","kind":"direct"}}"#
      )
      .expect("parse"),
      plain
    );
    let chosen = CallAction::join(
      request,
      JoinMedia {
        is_muted: true,
        is_camera_on: true,
      },
    );
    let json = r#"{"action":"join","request":{"conversation":"5","kind":"direct"},"is_muted":true,"is_camera_on":true}"#;
    assert_eq!(serde_json::to_string(&chosen).expect("serialize"), json);
    assert_eq!(serde_json::from_str::<CallAction>(json).expect("parse"), chosen);
    let camera_only: CallAction = serde_json::from_str(
      r#"{"action":"join","request":{"conversation":"5","kind":"direct"},"is_camera_on":true}"#,
    )
    .expect("parse");
    assert_eq!(
      camera_only,
      CallAction::Join {
        request,
        is_muted: false,
        is_camera_on: true,
        video_decoders: Vec::new(),
      }
    );
  }

  /// #442: the pre-join check and its camera's state have stable wire names; a snapshot without
  /// a preview (an older shell) has none running.
  #[test]
  fn the_pre_join_check_has_stable_wire_names() {
    let action = CallAction::SetPreview {
      preview: CallPreview {
        is_camera_on: true,
        is_mic_on: false,
      },
    };
    let json = r#"{"action":"set_preview","preview":{"is_camera_on":true,"is_mic_on":false}}"#;
    assert_eq!(serde_json::to_string(&action).expect("serialize"), json);
    assert_eq!(serde_json::from_str::<CallAction>(json).expect("parse"), action);
    assert_eq!(
      serde_json::from_str::<CallAction>(r#"{"action":"set_preview","preview":{}}"#).expect("parse"),
      CallAction::SetPreview {
        preview: CallPreview::default()
      }
    );
    for (camera, name) in [
      (PreviewCamera::Off, r#""off""#),
      (PreviewCamera::Starting, r#""starting""#),
      (PreviewCamera::Live, r#""live""#),
      (PreviewCamera::Failed, r#""failed""#),
    ] {
      assert_eq!(serde_json::to_string(&camera).expect("serialize"), name);
      assert_eq!(
        serde_json::from_str::<PreviewCamera>(name).expect("parse"),
        camera
      );
    }
    assert_eq!(
      serde_json::from_str::<PreviewCamera>(r#""paused""#).expect("parse"),
      PreviewCamera::Unknown
    );
    let without: CallSnapshot = serde_json::from_str(
      r#"{"incoming":[],"running":[],"devices":{"cameras":[],"microphones":[],"speakers":[]}}"#,
    )
    .expect("parse");
    assert_eq!(without.preview_camera, PreviewCamera::Off);
  }

  /// A call action answers `null`, or the failure of a part that didn't go through.
  #[test]
  fn a_call_action_answer_is_null_or_a_failure() {
    assert_eq!(
      serde_json::from_str::<CallActionAnswer>("null").expect("parse"),
      None
    );
    let failure = crate::failure::ActionFailure::new(
      crate::failure::FailureKind::Call(crate::failure::CallFailure::CameraInUse),
      "busy",
    )
    .to_wire();
    let answer: CallActionAnswer =
      serde_json::from_str(&serde_json::to_string(&failure).expect("serialize")).expect("parse");
    assert_eq!(answer, Some(failure));
  }

  #[test]
  fn video_actions_round_trip() {
    use crate::calls::BackgroundPreset;

    for action in [
      CallAction::SetCamera { is_on: true },
      CallAction::SetBackgroundEffect {
        effect: BackgroundEffect::Preset(BackgroundPreset::Office),
      },
      CallAction::StartScreenShare {
        has_sound: true,
        content_mode: ScreenContentMode::Motion,
        resolution: ScreenShareResolution::Uhd,
      },
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
  fn own_device_actions_have_stable_wire_names() {
    let device: DeviceId = crate::model::id_from_raw(7);
    let rename = ChatAction::RenameOwnDevice {
      device,
      name: "Work laptop".into(),
    };
    let json = serde_json::to_string(&rename).expect("serialize");
    assert_eq!(
      json,
      r#"{"action":"rename_own_device","device":"7","name":"Work laptop"}"#
    );
    assert_eq!(serde_json::from_str::<ChatAction>(&json).expect("parse"), rename);
    assert_eq!(
      serde_json::to_string(&ChatAction::TestPushOwnDevice { device }).expect("serialize"),
      r#"{"action":"test_push_own_device","device":"7"}"#
    );
  }

  #[test]
  fn the_backup_state_is_optional_in_the_security_snapshot() {
    let older: SecuritySnapshot =
      serde_json::from_str(r#"{"my_fingerprint":"ab","contacts":[]}"#).expect("parse");
    assert_eq!(older.maybe_backup, None);
    let snapshot = SecuritySnapshot {
      maybe_backup: Some(crate::security::KeyBackupState {
        is_stored: true,
        has_pending_upload: false,
        recovery: vec![crate::security::RecoveryMethod::Passphrase],
        maybe_passphrase_set_at: Some(1_000),
        ..crate::security::KeyBackupState::default()
      }),
      ..SecuritySnapshot::default()
    };
    let json = serde_json::to_value(&snapshot).expect("serialize");
    assert_eq!(
      json["backup"],
      serde_json::json!({"is_stored": true, "has_pending_upload": false, "recovery": ["passphrase"], "passphrase_set_at": 1000, "is_passphrase_allowed": true})
    );
    let older: crate::security::KeyBackupState =
      serde_json::from_str(r#"{"is_stored":true,"recovery":["passphrase"]}"#).expect("parse");
    assert!(older.is_passphrase_allowed);
    assert_eq!(older.maybe_recovery_key_set_at, None);
    let kit: crate::security::KeyBackupState =
      serde_json::from_str(r#"{"is_stored":true,"recovery":["recovery_key"],"recovery_key_set_at":5}"#)
        .expect("parse");
    assert!(kit.has(crate::security::RecoveryMethod::RecoveryKey));
    assert_eq!(kit.maybe_recovery_key_set_at, Some(5));
    let newer: crate::security::KeyBackupState =
      serde_json::from_str(r#"{"is_stored":true,"recovery":["passphrase","hardware_key"]}"#).expect("parse");
    assert_eq!(
      newer.recovery,
      [
        crate::security::RecoveryMethod::Passphrase,
        crate::security::RecoveryMethod::Unknown
      ]
    );
  }

  #[test]
  fn enroll_secrets_are_tagged_and_never_debug_printed() {
    let request = EnrollRequest {
      secret: EnrollSecret::RecoveryKey {
        recovery_key: "AAAA-BBBB".into(),
      },
      device_name: "Laptop".into(),
    };
    let json = serde_json::to_string(&request).expect("serialize");
    assert_eq!(
      json,
      r#"{"secret":{"kind":"recovery_key","recovery_key":"AAAA-BBBB"},"device_name":"Laptop"}"#
    );
    assert_eq!(
      serde_json::from_str::<EnrollRequest>(&json).expect("parse"),
      request
    );
    assert!(!format!("{request:?}").contains("AAAA"));
    let passphrase = EnrollSecret::Passphrase {
      passphrase: "hunter2 hunter2".into(),
    };
    assert!(!format!("{passphrase:?}").contains("hunter2"));
    assert_eq!(
      serde_json::to_string(&EnrollSecret::KeptOnDevice).expect("serialize"),
      r#"{"kind":"kept_on_device"}"#
    );
    assert_eq!(
      serde_json::to_string(&ChatAction::SetRecoveryKey {
        recovery_key: "K".into()
      })
      .expect("serialize"),
      r#"{"action":"set_recovery_key","recovery_key":"K"}"#
    );
  }

  #[test]
  fn device_actions_have_stable_wire_names() {
    assert_eq!(
      serde_json::to_string(&CallAction::RefreshDevices).expect("serialize"),
      r#"{"action":"refresh_devices"}"#
    );
    assert_eq!(
      serde_json::to_string(&CallAction::TestSpeakers {
        maybe_device: Some("spk-1".into())
      })
      .expect("serialize"),
      r#"{"action":"test_speakers","maybe_device":"spk-1"}"#
    );
    assert_eq!(
      serde_json::from_str::<CallAction>(r#"{"action":"test_speakers"}"#).expect("parse"),
      CallAction::TestSpeakers { maybe_device: None }
    );
  }

  #[test]
  fn screen_share_without_a_sound_flag_shares_the_picture_only() {
    let older: CallAction = serde_json::from_str(r#"{"action":"start_screen_share"}"#).expect("parse");
    assert_eq!(
      older,
      CallAction::StartScreenShare {
        has_sound: false,
        content_mode: ScreenContentMode::Detail,
        resolution: ScreenShareResolution::Standard,
      }
    );
  }

  /// The desktop page tells its shell what it decodes (#565); older UIs say nothing: H.264 only.
  #[test]
  fn join_carries_the_pages_decoders_only_when_there_are_any() {
    let request = CallRequest {
      conversation: "5".parse().expect("id"),
      kind: crate::calls::CallKind::Direct,
    };
    let mut join = CallAction::join(request, JoinMedia::default());
    let plain = serde_json::to_string(&join).expect("serialize");
    assert!(!plain.contains("video_decoders"), "{plain}");
    if let CallAction::Join { video_decoders, .. } = &mut join {
      video_decoders.push(teamcraft_proto::VideoCodec::Av1);
    }
    let json = serde_json::to_string(&join).expect("serialize");
    assert!(json.ends_with(r#""video_decoders":["av1"]}"#), "{json}");
    assert_eq!(serde_json::from_str::<CallAction>(&json).expect("parse"), join);
  }

  #[test]
  fn status_is_tagged_by_state() {
    let status = CoreStatus::NeedsEnrollment {
      has_existing_identity: true,
      was_data_lost: false,
      can_restore_on_device: false,
    };
    assert_eq!(
      serde_json::to_string(&status).expect("serialize"),
      r#"{"state":"needs_enrollment","has_existing_identity":true}"#
    );
    let lost: CoreStatus = serde_json::from_str(
      r#"{"state":"needs_enrollment","has_existing_identity":true,"was_data_lost":true,"can_restore_on_device":true}"#,
    )
    .expect("parse");
    assert_eq!(
      lost,
      CoreStatus::NeedsEnrollment {
        has_existing_identity: true,
        was_data_lost: true,
        can_restore_on_device: true,
      }
    );
    assert_eq!(
      serde_json::to_string(&CoreStatus::Stopped).expect("serialize"),
      r#"{"state":"stopped"}"#
    );
    let unreadable = CoreStatus::StoreUnreadable {
      problem: StoreProblem::NewerVersion,
    };
    assert_eq!(
      serde_json::to_string(&unreadable).expect("serialize"),
      r#"{"state":"store_unreadable","problem":"newer_version"}"#
    );
    let newer: CoreStatus =
      serde_json::from_str(r#"{"state":"store_unreadable","problem":"locked"}"#).expect("parse");
    assert_eq!(
      newer,
      CoreStatus::StoreUnreadable {
        problem: StoreProblem::Unknown
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
