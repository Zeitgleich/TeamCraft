//! Records and enums crossing the FFI. IDs are decimal snowflake strings: Kotlin and Swift have no
//! 64-bit unsigned type that matches, and the app only passes them back.

use teamcraft_client_api::model::{self as view, ConversationKind as ConversationKindView, PresenceStatus};
use teamcraft_core::UnreadableStore;
use teamcraft_core::client::LinkRequestInfo;
use teamcraft_core::device_activity::AppUse;
use teamcraft_core::notified::{
  NotifiedContent, NotifiedConversation as NotifiedConversationCore, NotifiedMessage as NotifiedMessageCore,
};
use teamcraft_core::notify_rules::MentionPreferences;
use teamcraft_core::pending_work::PendingWork;
use teamcraft_core::prefetch::SegmentPrefetch;
use teamcraft_core::push_guard::{PushDiagnostics, Suppression};
use teamcraft_core::push_preview::{AttachmentKind, PreviewContent, PreviewGap, PushPreview};
use teamcraft_proto::{
  DeviceLinkPlatform, Id, PushContentMode, PushKind, PushProvider, TestPushOutcome as TestPushOutcomeProto,
  TestPushResponse,
};

/// Where the app keeps an account's data.
#[derive(uniffi::Record)]
pub struct AppConfig {
  /// The TeamCraft server, e.g. `https://chat.example.com`.
  pub server_url: String,
  /// The app's private files directory.
  pub data_dir: String,
  /// Separates accounts on one device (the OIDC subject is a good choice).
  pub account: String,
  /// 32 random bytes the app keeps wrapped in the Android Keystore (e2ee.md §9.2).
  pub database_key: Vec<u8>,
  /// The push gateway this app was built for (a publisher's store app): device tokens for APNs and
  /// FCM are sealed for it. `None` for self-built apps, which use the gateway their server names.
  #[uniffi(default = None)]
  pub push_gateway_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AccountState {
  /// Enroll (a new identity, or recovery when `has_existing_identity`) or link this device.
  NeedsEnrollment { has_existing_identity: bool },
  Ready {
    user_id: String,
    display_name: String,
    /// Shown for out-of-band comparison (safety number).
    identity_fingerprint: String,
  },
  /// The local data can't be read by this version, and a retry won't change that (issue #414):
  /// the app offers to set the device up again (`TeamCraft::reset_store`), which sets the file
  /// aside instead of deleting it.
  StoreUnreadable { problem: StoreProblem },
}

/// Why the local data can't be read ([`AccountState::StoreUnreadable`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum StoreProblem {
  /// The file is damaged, or the key that opens it is gone (the keystore lost or replaced it).
  Damaged,
  /// A newer version of the app changed it: update the app.
  NewerVersion,
}

/// What the user does with the app, for presence (`TeamCraft::report_activity`, issue #472).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum AppActivity {
  /// The app came to the foreground (opened, resumed, unlocked).
  Foreground,
  /// The user touched the screen or pressed a key.
  Interaction,
  /// The app went to the background.
  Background,
}

/// Where the gateway connection stands. The banner follows this, not the OS connectivity: the
/// device can be online while the server is unreachable or the session expired.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ConnectionState {
  Connecting,
  Connected,
  /// Connected, but the server answers late (a saturated or very slow link): requests still
  /// complete. The banner says "slow", not "unreachable".
  Slow,
  /// Not connected; the next attempt starts at `retry_at_ms` (Unix milliseconds).
  /// `maybe_interception` names what answered instead of the server, if known.
  Offline {
    retry_at_ms: i64,
    maybe_interception: Option<NetworkInterception>,
  },
  /// The server refused the access token: the user has to sign in again.
  SignInRequired,
}

/// An HTTP proxy the platform reports (see `set_network_proxy`).
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct NetworkProxy {
  pub host: String,
  pub port: u16,
  /// Hosts that bypass it (`example.com`, `*.example.com`, `10.0.0.0/8`).
  pub exclusions: Vec<String>,
}

/// Something on the network that answered instead of the server (issue #157).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NetworkInterception {
  /// The network's login page (hotel, train, guest Wi-Fi): the user signs in to it first.
  CaptivePortal,
  /// A certificate the device doesn't trust, usually a proxy that inspects encrypted traffic.
  UntrustedCertificate,
}

/// What a device that asks to be linked runs on (e2ee.md §5); shown in the prompt on the user's
/// other devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LinkPlatform {
  Android,
  Ios,
  Macos,
  Windows,
  Linux,
  Web,
  Other,
}

/// A new device of the user that asks to be linked and waits for a decision.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LinkRequestItem {
  /// Names the request in `review_link_request` and `reject_link_request`.
  pub id: String,
  pub device_name: String,
  pub platform: LinkPlatform,
  /// Seconds the request stays open, when it was read.
  pub seconds_left: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ConversationKind {
  Public,
  Private,
  Direct,
  Group,
  Integration,
}

/// Whether the menu of a channel or space offers "Leave" (#1105); the core's rule, so the apps
/// don't re-implement it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum LeaveChoice {
  /// Not offered (DMs, group DMs).
  Hidden,
  Allowed,
  /// The user is the last admin: the entry is disabled and says to hand the role on first.
  LastAdmin,
}

impl From<teamcraft_client_api::entry_rules::LeaveChoice> for LeaveChoice {
  fn from(choice: teamcraft_client_api::entry_rules::LeaveChoice) -> Self {
    use teamcraft_client_api::entry_rules::LeaveChoice as Rule;
    match choice {
      Rule::Hidden => Self::Hidden,
      Rule::Allowed => Self::Allowed,
      Rule::LastAdmin => Self::LastAdmin,
    }
  }
}

/// What the menu of a space offers beyond what the conversation list already knows.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SpaceMenuItem {
  pub name: String,
  pub has_unread: bool,
  pub leave: LeaveChoice,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ConversationItem {
  pub id: String,
  pub maybe_space_id: Option<String>,
  pub maybe_space_name: Option<String>,
  pub kind: ConversationKind,
  pub name: String,
  pub topic: String,
  pub unread_count: u32,
  pub mention_count: u32,
  pub is_favorite: bool,
  pub is_muted: bool,
  pub can_post: bool,
  /// The other person's presence in a one-to-one DM (the direct messages list shows it).
  #[uniffi(default = None)]
  pub maybe_peer_presence: Option<Presence>,
  // What the user may do here besides posting, as the core decided it from the server's
  // `my_permissions` (issue #758): the apps offer an action only when it is allowed. Defaulted
  // (to offered) for items the apps build themselves; the server checks every request again.
  /// React to messages (`add_reaction`).
  #[uniffi(default = true)]
  pub can_react: bool,
  /// Pin and unpin messages (`manage_bookmarks`).
  #[uniffi(default = true)]
  pub can_manage_bookmarks: bool,
  /// Change the channel itself, such as archiving it (`manage_channel_properties`); never in DMs.
  #[uniffi(default = true)]
  pub can_manage_channel: bool,
  /// The other person in a one-to-one DM, so the list can show their profile picture.
  #[uniffi(default = None)]
  pub maybe_peer_id: Option<String>,
  /// The user's notes to themself ("Notes" in the lists, issue #829), decided by the core from
  /// the DM's participants; its `name` is empty, the apps name it in the user's language.
  #[uniffi(default = false)]
  pub is_notes_to_self: bool,
  /// When the newest top-level message was sent; filled only where the order needs it
  /// ([`share_targets`]), `None` elsewhere and for conversations without messages.
  #[uniffi(default = None)]
  pub maybe_last_activity_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MessageItem {
  pub id: String,
  pub conversation_id: String,
  pub author_id: String,
  pub author_name: String,
  pub sent_at_ms: i64,
  pub body: String,
  pub is_edited: bool,
  /// Written here, not yet accepted by the server.
  pub is_pending: bool,
  pub reply_count: u32,
  /// Set on replies: the thread (root message ID) they belong to.
  pub maybe_thread_id: Option<String>,
  pub reactions: Vec<ReactionItem>,
  /// The author's account was deleted (issue #630): the app shows its own "Deleted user" instead of
  /// `author_name`, the server's placeholder (issue #751). Defaulted, so items the apps
  /// build in tests need no change.
  #[uniffi(default = false)]
  pub is_author_deleted: bool,
  /// What a system message reports (a join, a rename), shown as a line instead of a bubble; `None`
  /// for messages people wrote. Membership and channel changes by someone else come with the
  /// message's extras (`maybe_workspace_event`). Defaulted, so items the apps build need no change.
  #[uniffi(default = None)]
  pub maybe_system_event: Option<SystemEventItem>,
  /// Set while this device can't read the message (issue #888): the app shows a bubble with the
  /// placeholder's text instead of `body` (empty then). It fills in place once readable.
  /// Defaulted, so items the apps build need no change.
  #[uniffi(default = None)]
  pub maybe_placeholder: Option<PlaceholderItem>,
}

/// Why a message shows as a placeholder ([`MessageItem::maybe_placeholder`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PlaceholderItem {
  /// "Waiting for this message", with a subtle pending mark: its key may still come.
  Waiting,
  /// "Not available on this device": no key is expected here.
  NotAvailable,
  /// "This message couldn't be decrypted".
  Failed,
}

/// How far conversations with messages waiting for keys have caught up (issue #888).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct KeyCatchUpItem {
  pub caught_up: u32,
  pub total: u32,
}

/// What still waits for the server (issue #158): the app keeps background time while it isn't
/// empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PendingWorkItem {
  /// Messages in the outbox that will be sent again (not those the server refused for good).
  pub sends: u32,
  /// Messages whose files are still going up.
  pub uploads: u32,
  /// Read positions, pins, presence and the like.
  pub actions: u32,
  pub is_empty: bool,
}

impl From<PendingWork> for PendingWorkItem {
  fn from(work: PendingWork) -> Self {
    let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    Self {
      sends: count(work.sends),
      uploads: count(work.uploads),
      actions: count(work.actions),
      is_empty: work.is_empty(),
    }
  }
}

/// The newest messages of a conversation down to the oldest one a list holds, plus the next older
/// page ([`crate::TeamCraft::messages_from`], issue #1058).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MessagePage {
  /// Oldest first.
  pub messages: Vec<MessageItem>,
  /// Older messages exist: the list can page further back.
  pub has_older: bool,
}

/// The system events of [`MessageItem::maybe_system_event`]; the message's author is who joined,
/// left, renamed or changed the topic (issue #769).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SystemEventItem {
  Joined,
  Left,
  Renamed { name: String },
  TopicChanged { topic: String },
}

/// An emoji the composer offers for a typed `:name`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct EmojiSuggestion {
  pub emoji: String,
  /// Its name without colons (`thumbsup`).
  pub shortcode: String,
}

/// What the sign-in screen shows of the server's brand.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct LoginBrand {
  pub display_name: String,
  /// The server serves its built-in brand (no brand configured): the screens show TeamCraft's own
  /// look, not a second tile beside it.
  pub is_product: bool,
  /// The brand's square icon, for its tile beside TeamCraft's (issue #463).
  pub maybe_icon_url: Option<String>,
  /// The dark theme's icon (the light one when the brand has no dark variant).
  pub maybe_icon_dark_url: Option<String>,
  pub maybe_logo_url: Option<String>,
  /// The dark theme's logo (the light one when the brand has no dark variant).
  pub maybe_logo_dark_url: Option<String>,
  /// Heading and text in the asked language (else English).
  pub maybe_title: Option<String>,
  pub maybe_text: Option<String>,
  pub links: BrandLinks,
  /// The brand's accent colors (checked `#rgb`/`#rrggbb`) for the light theme.
  pub light: BrandAccent,
  /// The same for the dark theme.
  pub dark: BrandAccent,
}

/// The brandable accent tokens (branding.md §4); absent keys keep the app's own colors.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct BrandAccent {
  pub maybe_accent: Option<String>,
  pub maybe_accent_strong: Option<String>,
  pub maybe_on_accent: Option<String>,
  pub maybe_accent_soft: Option<String>,
}

/// The brand's legal, support and documentation links; https only.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct BrandLinks {
  pub maybe_support: Option<String>,
  /// The instance's user documentation (Help & about, issue #410).
  pub maybe_documentation: Option<String>,
  pub maybe_privacy: Option<String>,
  pub maybe_terms: Option<String>,
  pub maybe_imprint: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ReactionItem {
  pub emoji: String,
  pub count: u32,
  pub reacted_by_me: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SearchHitItem {
  pub message: MessageItem,
  pub conversation_name: String,
  pub maybe_space_name: Option<String>,
}

/// What a wake-up asks the app to do (notifications.md, "Mobile push delivery").
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PushNotificationKind {
  /// Show a message notification (Android: the conversation's MessagingStyle notification).
  Message,
  /// Report an incoming call (Android: full-screen notification, iOS: CallKit).
  Call,
  /// Remove what was shown: the conversation (or thread) up to `maybe_message_id` was read on
  /// another device, or, with `maybe_call_id`, the ring ended. Never shows anything.
  Dismiss,
  /// The diagnostics' test push: show a short confirmation on the "other" channel.
  Test,
  /// A kind this version doesn't know: show nothing.
  Unknown,
}

/// Why the device keeps a push silent although the server sent it (its own second check).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NotificationSuppression {
  Muted,
  DoNotDisturb,
}

/// How much a message notification shows: the app's privacy setting, and (as a limit) the
/// workspace policy. The stricter of the two applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NotificationContent {
  /// Sender, conversation and the beginning of the text.
  Content,
  /// Sender and conversation; "New message" instead of the text.
  SenderOnly,
  /// "New message" only.
  Nothing,
}

/// The device's own setting and the limit the workspace policy sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NotificationContentSettings {
  pub device: NotificationContent,
  pub workspace: NotificationContent,
}

/// What kind of file a pushed message carries, for a label instead of the file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NotificationAttachmentKind {
  Image,
  Video,
  Audio,
  File,
}

/// Why a message notification shows less than the message (for the diagnostics; never content).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum NotificationGap {
  /// Signed out or no access token: the message could not be fetched.
  NoSession,
  /// Fetching, decrypting or verifying failed (offline, keys not arrived yet).
  NotDecrypted,
  /// Fetching took longer than the notification's time budget.
  TimedOut,
  /// Not a text or a file.
  NotShowable,
  /// The workspace policy shows less.
  WorkspacePolicy,
  /// This device's setting shows less.
  DeviceSetting,
}

/// What the device saw of pushes, for the notification diagnostics screen.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PushDiagnosticsInfo {
  pub received_count: u64,
  pub maybe_last_received_at_ms: Option<i64>,
  pub maybe_last_kind: Option<PushNotificationKind>,
  /// From the server sending to the device opening the last push.
  pub maybe_last_delay_ms: Option<i64>,
  pub maybe_last_test_at_ms: Option<i64>,
}

/// How the push service took a test push.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TestPushOutcome {
  Sent,
  NotRegistered,
  NotConfigured,
  Unavailable,
  Rejected,
  /// The registration was dead; register again.
  Gone,
  Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TestPushResult {
  pub outcome: TestPushOutcome,
  /// `apns`, `fcm`, `unifiedpush` or `webpush` as the server knows the registration.
  pub maybe_provider: Option<String>,
  pub sent_at_ms: i64,
}

/// What a notification shows; the app formats it (MessagingStyle on Android) and translates the
/// fallback texts.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PushNotification {
  pub kind: PushNotificationKind,
  pub conversation_id: String,
  pub maybe_message_id: Option<String>,
  pub maybe_conversation_name: Option<String>,
  pub maybe_sender_id: Option<String>,
  pub maybe_sender_name: Option<String>,
  /// The decrypted text, if the message could be read.
  pub maybe_text: Option<String>,
  /// A file's name, if the message is a file.
  pub maybe_file_name: Option<String>,
  /// What kind of file, if the message is a file.
  pub maybe_attachment_kind: Option<NotificationAttachmentKind>,
  /// Why less than the message is shown, if it is.
  pub maybe_gap: Option<NotificationGap>,
  pub is_direct: bool,
  pub is_mention: bool,
  pub is_urgent: bool,
  /// A ring, not a message: the app reports an incoming call instead of showing text.
  pub is_call: bool,
  /// Show without sound or vibration (the sender sent it silently).
  pub is_silent: bool,
  /// The message is known to belong to `conversation_id` (found locally, or decrypted and
  /// signature-checked there). Offer reply and mark-read only then: the conversation of a
  /// wake-up alone is the server's word.
  pub is_conversation_verified: bool,
  /// The thread the message replies in, or (dismiss) the thread that was read.
  pub maybe_thread_id: Option<String>,
  /// The ringing call, or (dismiss) the call whose ring ended.
  pub maybe_call_id: Option<String>,
  /// What the app icon badge should show now.
  pub maybe_badge: Option<u32>,
  /// When the server sent the push (order notifications by message time, not by arrival).
  pub maybe_sent_at_ms: Option<i64>,
  /// The message was shown before: don't post or alert again (urgent repeats are never duplicates).
  pub is_duplicate: bool,
  /// The device's own check of mutes and DND says: show nothing (iOS without the filtering
  /// entitlement: show it silently).
  pub maybe_suppression: Option<NotificationSuppression>,
}

/// What the newest notification showed of a conversation (issue #1065): opening it from the
/// notification shows this while the device hasn't caught the conversation up yet, never an empty
/// screen without a name.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NotifiedConversation {
  pub conversation_id: String,
  /// Channel name, or the other participants of a DM.
  pub maybe_name: Option<String>,
  pub is_direct: bool,
  /// The notified message, while the store doesn't have it yet.
  pub maybe_message: Option<NotifiedMessage>,
}

/// The message a notification showed, as far as it could be read.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NotifiedMessage {
  pub message_id: String,
  pub maybe_sender_id: Option<String>,
  pub maybe_sender_name: Option<String>,
  /// The decrypted text, if the message could be read.
  pub maybe_text: Option<String>,
  /// A file's name, if the message is a file.
  pub maybe_file_name: Option<String>,
  /// The thread it replies in: then it is not part of the conversation's own feed.
  pub maybe_thread_id: Option<String>,
  /// When it was sent (the server's clock, from the message ID).
  pub sent_at_ms: i64,
}

/// Device-local mention preferences (as on desktop): the server routes push by explicit mentions,
/// these are applied to the decrypted text on the device.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct MentionSettings {
  pub is_first_name_mention: bool,
  pub do_channel_mentions_notify: bool,
  /// Words that count as a mention (case-insensitive, matched anywhere in the text).
  pub keywords: Vec<String>,
}

/// What a search segment prefetch did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SegmentPrefetchResult {
  pub fetched: u32,
  /// Left out because they would exceed the size cap.
  pub skipped: u32,
  /// Bytes of the segments kept on this device.
  pub stored_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PushService {
  Fcm,
  UnifiedPush,
  /// Apple Push Notification service (the iOS app; the token is the hex device token).
  Apns,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Presence {
  Online,
  Away,
  DoNotDisturb,
  Offline,
}

impl ConversationItem {
  /// The list entry of a conversation; the caller looks up the space's name and the DM peer.
  /// `is_guest` is the user's, for what a conversation offers when an older server didn't say
  /// (issue #758).
  pub(crate) fn from_view(
    conversation: view::Conversation,
    maybe_space_name: Option<String>,
    maybe_peer: Option<DirectPeer>,
    is_guest: bool,
  ) -> Self {
    Self {
      can_react: conversation.can_react(),
      can_manage_bookmarks: conversation.can_manage_bookmarks(is_guest),
      can_manage_channel: conversation.can_manage_channel(),
      id: conversation.id.to_string(),
      maybe_space_id: conversation.maybe_space.map(|space| space.to_string()),
      maybe_space_name,
      kind: ConversationKind::from_view(conversation.kind),
      name: conversation.name,
      topic: conversation.topic,
      unread_count: conversation.unread_count,
      mention_count: conversation.mention_count,
      is_favorite: conversation.is_favorite,
      is_muted: conversation.is_muted,
      can_post: conversation.can_post,
      maybe_peer_presence: maybe_peer.as_ref().and_then(|peer| peer.maybe_presence),
      maybe_peer_id: maybe_peer.map(|peer| peer.id.to_string()),
      is_notes_to_self: conversation.is_notes_to_self,
      maybe_last_activity_ms: None,
    }
  }
}

/// Where a share from another app can go (issue #829): the conversations the user may post in,
/// their notes first, then the most recently active, then by name.
pub(crate) fn share_targets(items: Vec<ConversationItem>) -> Vec<ConversationItem> {
  let mut targets: Vec<ConversationItem> = items.into_iter().filter(|item| item.can_post).collect();
  targets.sort_by(|a, b| {
    b.is_notes_to_self
      .cmp(&a.is_notes_to_self)
      .then_with(|| b.maybe_last_activity_ms.cmp(&a.maybe_last_activity_ms))
      .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
  });
  targets
}

/// The other person in a one-to-one DM; `maybe_presence` is `None` while they are not known yet.
pub(crate) struct DirectPeer {
  pub(crate) id: view::UserId,
  pub(crate) maybe_presence: Option<Presence>,
}

impl ConversationKind {
  pub(crate) fn from_view(kind: ConversationKindView) -> Self {
    match kind {
      ConversationKindView::Public => Self::Public,
      ConversationKindView::Private => Self::Private,
      ConversationKindView::Direct => Self::Direct,
      ConversationKindView::Group => Self::Group,
      ConversationKindView::Integration => Self::Integration,
    }
  }
}

impl MessageItem {
  /// `maybe_author` is looked up by the caller (the view model only has the ID); an unknown author
  /// has no name yet.
  pub(crate) fn from_view(
    message: &view::Message,
    maybe_author: Option<&view::User>,
    is_pending: bool,
  ) -> Self {
    Self {
      id: message.id.to_string(),
      conversation_id: message.conversation.to_string(),
      author_id: message.author.to_string(),
      author_name: match message.maybe_imported.as_ref().map(|mark| &mark.author) {
        // An imported bot's or webhook's name; the apps don't know imported messages yet.
        Some(view::ImportedAuthorView::Named { name }) => name.clone(),
        _ => maybe_author
          .map(|author| author.display_name.clone())
          .unwrap_or_default(),
      },
      sent_at_ms: message.sent_at,
      body: message.body.clone(),
      is_edited: message.is_edited,
      is_pending,
      reply_count: message.reply_count,
      maybe_thread_id: message.maybe_thread.map(|thread| thread.to_string()),
      maybe_system_event: SystemEventItem::of(&message.kind),
      maybe_placeholder: message.maybe_placeholder.map(|placeholder| match placeholder {
        view::Placeholder::Waiting => PlaceholderItem::Waiting,
        view::Placeholder::NotAvailable | view::Placeholder::Unknown => PlaceholderItem::NotAvailable,
        view::Placeholder::Failed => PlaceholderItem::Failed,
      }),
      reactions: message
        .reactions
        .iter()
        .map(|reaction| ReactionItem {
          emoji: reaction.emoji.clone(),
          count: reaction.count,
          reacted_by_me: reaction.reacted_by_me,
        })
        .collect(),
      // Somebody without an account here is shown like a deleted account (e2ee.md §4.10).
      is_author_deleted: maybe_author.is_some_and(|author| author.is_deleted)
        || matches!(
          message.maybe_imported.as_ref().map(|mark| &mark.author),
          Some(view::ImportedAuthorView::Deleted)
        ),
    }
  }
}

impl SystemEventItem {
  fn of(kind: &view::MessageKind) -> Option<Self> {
    let view::MessageKind::System(event) = kind else {
      return None;
    };
    Some(match event {
      view::SystemEvent::Joined => Self::Joined,
      view::SystemEvent::Left => Self::Left,
      view::SystemEvent::Renamed { name } => Self::Renamed { name: name.clone() },
      view::SystemEvent::TopicChanged { topic } => Self::TopicChanged { topic: topic.clone() },
    })
  }
}

impl From<PushPreview> for PushNotification {
  fn from(preview: PushPreview) -> Self {
    let (maybe_text, maybe_file_name, maybe_attachment_kind) = match preview.content {
      PreviewContent::Text(text) => (Some(text), None, None),
      PreviewContent::File { file_name, kind } => (None, Some(file_name), Some(kind.into())),
      PreviewContent::Unavailable => (None, None, None),
    };
    let wake_up = &preview.wake_up;
    Self {
      kind: notification_kind(&wake_up.kind),
      maybe_thread_id: wake_up.maybe_thread_id.map(|id| id.to_string()),
      maybe_call_id: wake_up.maybe_call_id.map(|id| id.to_string()),
      maybe_badge: wake_up.maybe_badge,
      maybe_sent_at_ms: wake_up.maybe_sent_at.map(|at| at.0),
      is_duplicate: false,
      maybe_suppression: None,
      conversation_id: preview.wake_up.conversation_id.to_string(),
      maybe_message_id: preview.wake_up.maybe_message_id.map(|id| id.to_string()),
      maybe_conversation_name: preview.maybe_conversation_name,
      maybe_sender_id: preview.wake_up.maybe_sender_id.map(|id| id.to_string()),
      maybe_sender_name: preview.maybe_sender_name,
      maybe_text,
      maybe_file_name,
      maybe_attachment_kind,
      maybe_gap: preview.maybe_gap.map(Into::into),
      is_direct: preview.is_direct,
      is_mention: preview.wake_up.kind == PushKind::Mention,
      is_urgent: preview.wake_up.is_urgent,
      is_call: preview.wake_up.kind == PushKind::Call,
      is_silent: preview.is_silent,
      is_conversation_verified: preview.is_conversation_verified,
    }
  }
}

impl From<NotifiedConversationCore> for NotifiedConversation {
  fn from(notified: NotifiedConversationCore) -> Self {
    Self {
      conversation_id: notified.conversation_id.to_string(),
      maybe_name: notified.maybe_name,
      is_direct: notified.is_direct,
      maybe_message: notified.maybe_message.map(Into::into),
    }
  }
}

impl From<NotifiedMessageCore> for NotifiedMessage {
  fn from(message: NotifiedMessageCore) -> Self {
    let (maybe_text, maybe_file_name) = match message.content {
      NotifiedContent::Text { text } => (Some(text), None),
      NotifiedContent::File { file_name } => (None, Some(file_name)),
      NotifiedContent::Unavailable => (None, None),
    };
    Self {
      message_id: message.message_id.to_string(),
      maybe_sender_id: message.maybe_sender_id.map(|id| id.to_string()),
      maybe_sender_name: message.maybe_sender_name,
      maybe_text,
      maybe_file_name,
      maybe_thread_id: message.maybe_thread_id.map(|id| id.to_string()),
      sent_at_ms: message.message_id.snowflake().unix_millis(),
    }
  }
}

impl From<AttachmentKind> for NotificationAttachmentKind {
  fn from(kind: AttachmentKind) -> Self {
    match kind {
      AttachmentKind::Image => Self::Image,
      AttachmentKind::Video => Self::Video,
      AttachmentKind::Audio => Self::Audio,
      AttachmentKind::Other => Self::File,
    }
  }
}

impl From<PreviewGap> for NotificationGap {
  fn from(gap: PreviewGap) -> Self {
    match gap {
      PreviewGap::NoSession => Self::NoSession,
      PreviewGap::NotDecrypted => Self::NotDecrypted,
      PreviewGap::TimedOut => Self::TimedOut,
      PreviewGap::NotShowable => Self::NotShowable,
      PreviewGap::WorkspacePolicy => Self::WorkspacePolicy,
      PreviewGap::DeviceSetting => Self::DeviceSetting,
    }
  }
}

impl From<NotificationContent> for PushContentMode {
  fn from(content: NotificationContent) -> Self {
    match content {
      NotificationContent::Content => Self::Full,
      NotificationContent::SenderOnly => Self::Sender,
      NotificationContent::Nothing => Self::Generic,
    }
  }
}

impl From<&PushContentMode> for NotificationContent {
  /// A mode from a newer server counts as the strictest one.
  fn from(mode: &PushContentMode) -> Self {
    match mode {
      PushContentMode::Full => Self::Content,
      PushContentMode::Sender => Self::SenderOnly,
      PushContentMode::Generic | PushContentMode::Unknown(_) => Self::Nothing,
    }
  }
}

pub(crate) fn notification_kind(kind: &PushKind) -> PushNotificationKind {
  match kind {
    PushKind::Message | PushKind::Mention => PushNotificationKind::Message,
    PushKind::Call => PushNotificationKind::Call,
    PushKind::Dismiss => PushNotificationKind::Dismiss,
    PushKind::Test => PushNotificationKind::Test,
    PushKind::Unknown(_) => PushNotificationKind::Unknown,
  }
}

impl From<Suppression> for NotificationSuppression {
  fn from(suppression: Suppression) -> Self {
    match suppression {
      Suppression::Muted => Self::Muted,
      Suppression::DoNotDisturb => Self::DoNotDisturb,
    }
  }
}

impl From<PushDiagnostics> for PushDiagnosticsInfo {
  fn from(diagnostics: PushDiagnostics) -> Self {
    Self {
      received_count: diagnostics.received_count,
      maybe_last_received_at_ms: diagnostics.maybe_last_received_at_ms,
      maybe_last_kind: diagnostics.maybe_last_kind.as_ref().map(notification_kind),
      maybe_last_delay_ms: diagnostics.maybe_last_delay_ms,
      maybe_last_test_at_ms: diagnostics.maybe_last_test_at_ms,
    }
  }
}

impl From<TestPushResponse> for TestPushResult {
  fn from(response: TestPushResponse) -> Self {
    Self {
      outcome: match response.outcome {
        TestPushOutcomeProto::Sent => TestPushOutcome::Sent,
        TestPushOutcomeProto::NotRegistered => TestPushOutcome::NotRegistered,
        TestPushOutcomeProto::NotConfigured => TestPushOutcome::NotConfigured,
        TestPushOutcomeProto::Unavailable => TestPushOutcome::Unavailable,
        TestPushOutcomeProto::Rejected => TestPushOutcome::Rejected,
        TestPushOutcomeProto::Gone => TestPushOutcome::Gone,
        TestPushOutcomeProto::Unknown(_) => TestPushOutcome::Unknown,
      },
      maybe_provider: response
        .maybe_provider
        .map(|provider| provider.as_str().to_owned()),
      sent_at_ms: response.sent_at.0,
    }
  }
}

impl From<PushService> for PushProvider {
  fn from(service: PushService) -> Self {
    match service {
      PushService::Fcm => Self::Fcm,
      PushService::UnifiedPush => Self::UnifiedPush,
      PushService::Apns => Self::Apns,
    }
  }
}

impl From<PresenceStatus> for Presence {
  fn from(presence: PresenceStatus) -> Self {
    match presence {
      PresenceStatus::Online => Self::Online,
      PresenceStatus::Away => Self::Away,
      PresenceStatus::DoNotDisturb => Self::DoNotDisturb,
      PresenceStatus::Offline => Self::Offline,
    }
  }
}

impl From<Presence> for PresenceStatus {
  fn from(presence: Presence) -> Self {
    match presence {
      Presence::Online => Self::Online,
      Presence::Away => Self::Away,
      Presence::DoNotDisturb => Self::DoNotDisturb,
      Presence::Offline => Self::Offline,
    }
  }
}

/// A recovery request this device made: the server's ID and the code the user reads to an admin.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct OrgRecoveryStart {
  pub request_id: String,
  pub verification_code: String,
}

/// Desktop's defaults: the first name and `@channel` notify, no keywords.
impl Default for MentionSettings {
  fn default() -> Self {
    Self {
      is_first_name_mention: true,
      do_channel_mentions_notify: true,
      keywords: Vec::new(),
    }
  }
}

impl From<teamcraft_core::client::SegmentPrefetch> for SegmentPrefetchResult {
  fn from(prefetch: teamcraft_core::client::SegmentPrefetch) -> Self {
    Self {
      fetched: u32::try_from(prefetch.fetched).unwrap_or(u32::MAX),
      skipped: u32::try_from(prefetch.skipped).unwrap_or(u32::MAX),
      stored_bytes: prefetch.stored_bytes,
    }
  }
}

/// When the app downloads shared search index segments ahead of time (the core keeps the choice).
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SegmentPrefetchMode {
  Off,
  /// Only on networks that aren't metered.
  UnmeteredOnly,
  Always,
}

impl From<SegmentPrefetch> for SegmentPrefetchMode {
  fn from(mode: SegmentPrefetch) -> Self {
    match mode {
      SegmentPrefetch::Off => Self::Off,
      SegmentPrefetch::UnmeteredOnly => Self::UnmeteredOnly,
      SegmentPrefetch::Always => Self::Always,
    }
  }
}

impl From<SegmentPrefetchMode> for SegmentPrefetch {
  fn from(mode: SegmentPrefetchMode) -> Self {
    match mode {
      SegmentPrefetchMode::Off => Self::Off,
      SegmentPrefetchMode::UnmeteredOnly => Self::UnmeteredOnly,
      SegmentPrefetchMode::Always => Self::Always,
    }
  }
}

impl From<MentionPreferences> for MentionSettings {
  fn from(preferences: MentionPreferences) -> Self {
    Self {
      is_first_name_mention: preferences.first_name_is_mention,
      do_channel_mentions_notify: preferences.channel_mentions_notify,
      keywords: preferences.keywords,
    }
  }
}

impl From<MentionSettings> for MentionPreferences {
  fn from(settings: MentionSettings) -> Self {
    Self {
      first_name_is_mention: settings.is_first_name_mention,
      channel_mentions_notify: settings.do_channel_mentions_notify,
      keywords: settings.keywords,
    }
  }
}

impl From<teamcraft_core::connection::ConnectionState> for ConnectionState {
  fn from(state: teamcraft_core::connection::ConnectionState) -> Self {
    use teamcraft_core::api::Interception;
    use teamcraft_core::connection::ConnectionState as Core;
    match state {
      Core::Connecting => Self::Connecting,
      Core::Connected => Self::Connected,
      Core::Slow => Self::Slow,
      Core::Offline {
        retry_at_ms,
        maybe_interception,
      } => Self::Offline {
        retry_at_ms,
        maybe_interception: maybe_interception.map(|kind| match kind {
          Interception::CaptivePortal => NetworkInterception::CaptivePortal,
          Interception::UntrustedCertificate => NetworkInterception::UntrustedCertificate,
        }),
      },
      Core::SignInRequired { .. } => Self::SignInRequired,
    }
  }
}

impl From<UnreadableStore> for StoreProblem {
  fn from(problem: UnreadableStore) -> Self {
    match problem {
      UnreadableStore::Damaged => Self::Damaged,
      UnreadableStore::NewerVersion => Self::NewerVersion,
    }
  }
}

impl From<AppActivity> for AppUse {
  fn from(activity: AppActivity) -> Self {
    match activity {
      AppActivity::Foreground => Self::Foreground,
      AppActivity::Interaction => Self::Interaction,
      AppActivity::Background => Self::Background,
    }
  }
}

impl From<LinkPlatform> for DeviceLinkPlatform {
  fn from(platform: LinkPlatform) -> Self {
    match platform {
      LinkPlatform::Android => Self::Android,
      LinkPlatform::Ios => Self::Ios,
      LinkPlatform::Macos => Self::Macos,
      LinkPlatform::Windows => Self::Windows,
      LinkPlatform::Linux => Self::Linux,
      LinkPlatform::Web => Self::Web,
      LinkPlatform::Other => Self::Other,
    }
  }
}

impl From<DeviceLinkPlatform> for LinkPlatform {
  fn from(platform: DeviceLinkPlatform) -> Self {
    match platform {
      DeviceLinkPlatform::Android => Self::Android,
      DeviceLinkPlatform::Ios => Self::Ios,
      DeviceLinkPlatform::Macos => Self::Macos,
      DeviceLinkPlatform::Windows => Self::Windows,
      DeviceLinkPlatform::Linux => Self::Linux,
      DeviceLinkPlatform::Web => Self::Web,
      DeviceLinkPlatform::Other => Self::Other,
    }
  }
}

impl From<LinkRequestInfo> for LinkRequestItem {
  fn from(request: LinkRequestInfo) -> Self {
    Self {
      id: request.id,
      device_name: request.device_name,
      platform: request.platform.into(),
      seconds_left: request.seconds_left,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod system_events {
    use super::*;

    #[test]
    fn each_kind_reaches_the_apps_with_its_text() {
      let cases = [
        (view::SystemEvent::Joined, SystemEventItem::Joined),
        (view::SystemEvent::Left, SystemEventItem::Left),
        (
          view::SystemEvent::Renamed {
            name: "Project A".into(),
          },
          SystemEventItem::Renamed {
            name: "Project A".into(),
          },
        ),
        (
          view::SystemEvent::TopicChanged {
            topic: "Launch plan".into(),
          },
          SystemEventItem::TopicChanged {
            topic: "Launch plan".into(),
          },
        ),
      ];
      for (event, expected) in cases {
        let item = MessageItem::from_view(&message_of(view::MessageKind::System(event)), None, false);
        assert_eq!(item.maybe_system_event, Some(expected));
      }
    }

    /// A change (archived, an admin who joined) is read from the extras' workspace event, as the
    /// apps do already.
    #[test]
    fn written_messages_log_lines_and_changes_are_no_system_events() {
      for kind in [
        view::MessageKind::User,
        view::MessageKind::Log(view::LogLevel::Info),
        view::MessageKind::Change,
      ] {
        let item = MessageItem::from_view(&message_of(kind), None, false);
        assert_eq!(item.maybe_system_event, None);
      }
    }

    fn message_of(kind: view::MessageKind) -> view::Message {
      view::Message {
        id: view::id_from_raw(7),
        conversation: view::id_from_raw(5),
        author: view::id_from_raw(3),
        kind,
        sent_at: 0,
        body: String::new(),
        is_edited: false,
        reactions: Vec::new(),
        reply_count: 0,
        maybe_thread: None,
        maybe_imported: None,
        maybe_placeholder: None,
      }
    }
  }

  #[test]
  fn conversation_items_carry_what_the_user_may_do() {
    let item = |kind, maybe_permissions, is_guest| {
      let conversation = view::Conversation {
        id: view::id_from_raw(5),
        maybe_space: Some(view::id_from_raw(2)),
        kind,
        category: view::SidebarCategory::Channels,
        name: "news".into(),
        topic: "Daily".into(),
        member_ids: Vec::new(),
        unread_count: 3,
        mention_count: 1,
        is_favorite: false,
        is_muted: false,
        can_post: true,
        bookmarks: Vec::new(),
        is_catching_up: false,
        maybe_image_version: None,
        is_notes_to_self: false,
        maybe_permissions,
        has_only_deactivated_others: false,
      };
      ConversationItem::from_view(conversation, Some("Project A".into()), None, is_guest)
    };
    let rights = |item: &ConversationItem| {
      (
        item.can_post,
        item.can_react,
        item.can_manage_bookmarks,
        item.can_manage_channel,
      )
    };
    // The server's answer: a moderated channel where members may post but not react or pin.
    let moderated = item(
      ConversationKindView::Public,
      Some(vec![teamcraft_proto::Permission::CreatePost]),
      false,
    );
    assert_eq!(rights(&moderated), (true, false, false, false));
    assert_eq!(
      (moderated.id.as_str(), moderated.maybe_space_name.as_deref()),
      ("5", Some("Project A"))
    );
    // An older server: the built-in scheme, where guests keep no pins; DMs have no channel.
    assert_eq!(
      rights(&item(ConversationKindView::Private, None, false)),
      (true, true, true, true)
    );
    assert_eq!(
      rights(&item(ConversationKindView::Private, None, true)),
      (true, true, false, true)
    );
    assert_eq!(
      rights(&item(ConversationKindView::Direct, None, false)),
      (true, true, true, false)
    );
  }

  /// Issue #846: the list shows the DM peer's picture, so the item names the peer.
  #[test]
  fn a_direct_conversation_item_names_its_peer() {
    let conversation = view::Conversation {
      id: view::id_from_raw(5),
      maybe_space: None,
      kind: ConversationKindView::Direct,
      category: view::SidebarCategory::DirectMessages,
      name: "Alex Example".into(),
      topic: String::new(),
      member_ids: vec![view::id_from_raw(1), view::id_from_raw(42)],
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      maybe_image_version: None,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
    };
    let peer = DirectPeer {
      id: view::id_from_raw(42),
      maybe_presence: Some(Presence::Away),
    };
    let item = ConversationItem::from_view(conversation.clone(), None, Some(peer), false);
    assert_eq!(item.maybe_peer_id.as_deref(), Some("42"));
    assert_eq!(item.maybe_peer_presence, Some(Presence::Away));
    let unknown = ConversationItem::from_view(conversation, None, None, false);
    assert_eq!((unknown.maybe_peer_id, unknown.maybe_peer_presence), (None, None));
  }

  #[test]
  fn notification_content_maps_both_ways_and_unknown_modes_are_strictest() {
    for content in [
      NotificationContent::Content,
      NotificationContent::SenderOnly,
      NotificationContent::Nothing,
    ] {
      let mode: PushContentMode = content.into();
      assert_eq!(NotificationContent::from(&mode), content);
    }
    assert_eq!(
      NotificationContent::from(&PushContentMode::Unknown("later".into())),
      NotificationContent::Nothing
    );
  }

  #[test]
  fn a_deleted_author_is_flagged_for_the_apps_own_name() {
    let message = message();
    let mut author = author();
    assert!(!MessageItem::from_view(&message, Some(&author), false).is_author_deleted);

    author.is_deleted = true;
    author.display_name = "Deleted user".into();
    let item = MessageItem::from_view(&message, Some(&author), false);
    assert!(item.is_author_deleted);
    assert_eq!(item.author_name, "Deleted user");

    let unknown = MessageItem::from_view(&message, None, false);
    assert!(!unknown.is_author_deleted);
    assert_eq!(unknown.author_name, "");
  }

  /// Issue #829: the notes reach the apps marked as such (their name is empty).
  #[test]
  fn the_notes_item_is_marked() {
    let notes = ConversationItem::from_view(notes_view(), None, None, false);
    assert!(notes.is_notes_to_self);
    assert_eq!(notes.name, "");
    let dm = ConversationItem::from_view(conversation_view(6, "Alex Example"), None, None, false);
    assert!(!dm.is_notes_to_self);
  }

  mod share_targets {
    use super::*;

    #[test]
    fn notes_come_first_then_the_most_recent_then_by_name() {
      let item = |view, maybe_last_activity_ms| ConversationItem {
        maybe_last_activity_ms,
        ..ConversationItem::from_view(view, None, None, false)
      };
      let targets = share_targets(vec![
        item(conversation_view(1, "zeta"), None),
        item(conversation_view(2, "Alpha"), None),
        item(conversation_view(3, "old"), Some(100)),
        item(notes_view(), None),
        item(conversation_view(4, "new"), Some(200)),
      ]);
      let ids: Vec<&str> = targets.iter().map(|target| target.id.as_str()).collect();
      assert_eq!(ids, vec!["9", "4", "3", "2", "1"]);
    }

    #[test]
    fn conversations_the_user_may_not_post_in_are_left_out() {
      let read_only = view::Conversation {
        can_post: false,
        ..conversation_view(1, "announcements")
      };
      let targets = share_targets(vec![
        ConversationItem::from_view(read_only, None, None, false),
        ConversationItem::from_view(notes_view(), None, None, false),
      ]);
      assert_eq!(targets.len(), 1);
      assert!(targets[0].is_notes_to_self);
    }
  }

  fn message() -> view::Message {
    view::Message {
      id: view::id_from_raw(7),
      conversation: view::id_from_raw(5),
      author: view::id_from_raw(3),
      kind: view::MessageKind::User,
      sent_at: 0,
      body: "Hello".into(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
      maybe_imported: None,
      maybe_placeholder: None,
    }
  }

  fn author() -> view::User {
    view::User {
      id: view::id_from_raw(3),
      username: "sam".into(),
      display_name: "Sam Doe".into(),
      presence: PresenceStatus::Offline,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      is_deactivated: false,
      is_deleted: false,
      device_setup: view::DeviceSetup::Unknown,
      maybe_email: None,
    }
  }

  fn notes_view() -> view::Conversation {
    view::Conversation {
      kind: ConversationKindView::Direct,
      category: view::SidebarCategory::DirectMessages,
      member_ids: vec![view::id_from_raw(1)],
      is_notes_to_self: true,
      ..conversation_view(9, "")
    }
  }

  fn conversation_view(id: u64, name: &str) -> view::Conversation {
    view::Conversation {
      id: view::id_from_raw(id),
      maybe_space: None,
      kind: ConversationKindView::Public,
      category: view::SidebarCategory::Channels,
      name: name.into(),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      maybe_image_version: None,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
    }
  }
}
