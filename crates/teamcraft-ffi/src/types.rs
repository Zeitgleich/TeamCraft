//! Records and enums crossing the FFI. IDs are decimal snowflake strings: Kotlin and Swift have no
//! 64-bit unsigned type that matches, and the app only passes them back.

use teamcraft_client_api::model::{self as view, ConversationKind as ConversationKindView, PresenceStatus};
use teamcraft_core::notify_rules::MentionPreferences;
use teamcraft_core::prefetch::SegmentPrefetch;
use teamcraft_core::push_preview::{PreviewContent, PushPreview};
use teamcraft_proto::{PushKind, PushProvider};

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ConversationKind {
  Public,
  Private,
  Direct,
  Group,
  Integration,
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

/// What a notification shows; the app formats it (MessagingStyle on Android) and translates the
/// fallback texts.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct PushNotification {
  pub conversation_id: String,
  pub maybe_message_id: Option<String>,
  pub maybe_conversation_name: Option<String>,
  pub maybe_sender_id: Option<String>,
  pub maybe_sender_name: Option<String>,
  /// The decrypted text, if the message could be read.
  pub maybe_text: Option<String>,
  /// A file's name, if the message is a file.
  pub maybe_file_name: Option<String>,
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
  /// `author_name` is resolved by the caller (the view model only has the ID).
  pub(crate) fn from_view(message: &view::Message, author_name: String, is_pending: bool) -> Self {
    Self {
      id: message.id.to_string(),
      conversation_id: message.conversation.to_string(),
      author_id: message.author.to_string(),
      author_name,
      sent_at_ms: message.sent_at,
      body: message.body.clone(),
      is_edited: message.is_edited,
      is_pending,
      reply_count: message.reply_count,
      maybe_thread_id: message.maybe_thread.map(|thread| thread.to_string()),
      reactions: message
        .reactions
        .iter()
        .map(|reaction| ReactionItem {
          emoji: reaction.emoji.clone(),
          count: reaction.count,
          reacted_by_me: reaction.reacted_by_me,
        })
        .collect(),
    }
  }
}

impl From<PushPreview> for PushNotification {
  fn from(preview: PushPreview) -> Self {
    let (maybe_text, maybe_file_name) = match preview.content {
      PreviewContent::Text(text) => (Some(text), None),
      PreviewContent::File { file_name } => (None, Some(file_name)),
      PreviewContent::Unavailable => (None, None),
    };
    Self {
      conversation_id: preview.wake_up.conversation_id.to_string(),
      maybe_message_id: preview.wake_up.maybe_message_id.map(|id| id.to_string()),
      maybe_conversation_name: preview.maybe_conversation_name,
      maybe_sender_id: preview.wake_up.maybe_sender_id.map(|id| id.to_string()),
      maybe_sender_name: preview.maybe_sender_name,
      maybe_text,
      maybe_file_name,
      is_direct: preview.is_direct,
      is_mention: preview.wake_up.kind == PushKind::Mention,
      is_urgent: preview.wake_up.is_urgent,
      is_call: preview.wake_up.kind == PushKind::Call,
      is_silent: preview.is_silent,
      is_conversation_verified: preview.is_conversation_verified,
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
