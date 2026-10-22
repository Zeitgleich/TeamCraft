//! Records and enums crossing the FFI. IDs are decimal snowflake strings: Kotlin and Swift have no
//! 64-bit unsigned type that matches, and the app only passes them back.

use teamcraft_client_api::model::{self as view, ConversationKind as ConversationKindView, PresenceStatus};
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
  /// Show without sound or vibration (the sender sent it silently).
  pub is_silent: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum PushService {
  Fcm,
  UnifiedPush,
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
      is_silent: preview.is_silent,
    }
  }
}

impl From<PushService> for PushProvider {
  fn from(service: PushService) -> Self {
    match service {
      PushService::Fcm => Self::Fcm,
      PushService::UnifiedPush => Self::UnifiedPush,
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
