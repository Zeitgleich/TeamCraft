//! What a push notification shows (e2ee.md §7), worked out from the encrypted wake-up and the local
//! store alone, without a [`crate::Client`].
//!
//! Notification extensions and the browser's service worker run next to the app and must not
//! advance its state, so everything here only reads the store. With a [`ServerApi`] (a still valid
//! access token, see [`PushSession`]) the message is fetched and decrypted for a preview; without
//! one, or if that fails, the notification names the conversation and sender only.

use serde::{Deserialize, Serialize};
use teamcraft_crypto::push::open_wakeup;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  Base64Bytes, ConversationResponse, Id, ListMessagesQuery, MessageId, PushContentMode, PushKind, PushWakeUp,
  Snowflake, UserSummary,
};

use crate::api::ServerApi;
use crate::client::decrypt_content;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageContent, MessageFilter, Table};

pub(crate) const PUSH_KEY_RECORD: &str = "push_key";
const PUSH_SESSION_RECORD: &str = "push_session";
/// Longest text shown in a notification; the rest is cut at a character boundary.
pub const MAX_PREVIEW_CHARS: usize = 240;

/// The notification for one push.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushPreview {
  pub wake_up: PushWakeUp,
  /// Channel name, or the other participants of a DM.
  pub maybe_conversation_name: Option<String>,
  pub maybe_sender_name: Option<String>,
  pub content: PreviewContent,
  /// A DM or group DM: the sender's name is the title, no "sender:" prefix needed.
  pub is_direct: bool,
  /// The sender sent it silently: show it without sound or vibration.
  pub is_silent: bool,
  /// The message was found in, or decrypted and signature-checked as part of, the wake-up's
  /// conversation. Only then may the notification offer to reply there: the wake-up's
  /// conversation ID alone is the server's word (e2ee.md §7). Survives the policy limits.
  pub is_conversation_verified: bool,
}

/// What of the message itself can be shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewContent {
  Text(String),
  File {
    file_name: String,
  },
  /// Not decryptable here (no session, offline, keys not yet arrived) or not a showable kind.
  Unavailable,
}

/// Lets a notification process fetch messages while the app is closed: the server and a recent
/// access token. Stored in the (encrypted) local store; the app refreshes it when it refreshes its
/// token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSession {
  pub server_url: String,
  pub access_token: String,
  /// After this the token is not used.
  pub expires_at_ms: i64,
}

/// Opens a sealed push payload with the device's push key.
pub fn open_push_wakeup(store: &impl LocalStore, sealed: &[u8]) -> Result<PushWakeUp, CoreError> {
  let stored: Base64Bytes = records::get_json(store, Table::Account, PUSH_KEY_RECORD)?
    .ok_or_else(|| CoreError::Corrupt("no push key registered".into()))?;
  let key =
    SymmetricKey::from_slice(stored.as_slice()).ok_or_else(|| CoreError::Corrupt("bad push key".into()))?;
  let plaintext = open_wakeup(&key, sealed).map_err(|error| CoreError::Corrupt(error.to_string()))?;
  serde_json::from_slice(plaintext.expose_secret()).map_err(|error| CoreError::Corrupt(error.to_string()))
}

/// The notification for a sealed push payload. `maybe_api` is used to fetch the message if the
/// store doesn't have it yet.
pub async fn push_preview<A: ServerApi>(
  store: &impl LocalStore,
  maybe_api: Option<&A>,
  sealed: &[u8],
) -> Result<PushPreview, CoreError> {
  let wake_up = open_push_wakeup(store, sealed)?;
  let maybe_conversation: Option<ConversationResponse> =
    records::get_json(store, Table::Conversations, &wake_up.conversation_id.to_string())?;
  let is_direct = maybe_conversation
    .as_ref()
    .is_some_and(|conversation| !conversation.participants.is_empty());
  let maybe_conversation_name = maybe_conversation.as_ref().and_then(conversation_name);
  let maybe_sender_name = match wake_up.maybe_sender_id {
    Some(sender) => records::get_json::<UserSummary>(store, Table::Users, &sender.to_string())?
      .map(|user| user.display_name)
      .or_else(|| participant_name(maybe_conversation.as_ref(), sender)),
    None => None,
  };
  // Only messages have content to fetch; a withdrawal names the newest message read, not one to show.
  let (content, is_silent, is_conversation_verified) = match (&wake_up.kind, wake_up.maybe_message_id) {
    (PushKind::Message | PushKind::Mention, Some(message_id)) => {
      preview_content(store, maybe_api, &wake_up, message_id).await
    },
    // A ring names no message; answering joins that conversation's call, which the server controls
    // anyway. It counts as verified when this device knows the conversation as one it is in, so the
    // apps offer answer and decline.
    (PushKind::Call, _) => (
      PreviewContent::Unavailable,
      false,
      maybe_conversation
        .as_ref()
        .is_some_and(|conversation| conversation.maybe_my_membership.is_some()),
    ),
    _ => (PreviewContent::Unavailable, false, false),
  };
  let preview = PushPreview {
    wake_up,
    maybe_conversation_name,
    maybe_sender_name,
    content,
    is_direct,
    is_silent,
    is_conversation_verified,
  };
  Ok(preview.limited_to(&push_content_mode(store)))
}

/// What the verified workspace policy lets notifications show. Fails closed: with no verified
/// workspace state on this device yet, or one that cannot be read, a notification shows nothing
/// but "New message".
pub fn push_content_mode(store: &impl LocalStore) -> PushContentMode {
  match records::get_json::<crate::workspace::WorkspaceState>(
    store,
    Table::Workspace,
    crate::client::WORKSPACE_KEY,
  ) {
    Ok(Some(state)) => state.policy.notifications.push_content,
    Ok(None) | Err(_) => PushContentMode::Generic,
  }
}

impl PushPreview {
  /// The preview cut down to what the policy allows on a lock screen (admins' "push content" setting).
  /// An unknown mode from a newer server is treated as the strictest one.
  #[must_use]
  pub fn limited_to(mut self, mode: &PushContentMode) -> Self {
    match mode {
      PushContentMode::Full => {},
      PushContentMode::Sender => self.content = PreviewContent::Unavailable,
      PushContentMode::Generic | PushContentMode::Unknown(_) => {
        self.content = PreviewContent::Unavailable;
        self.maybe_conversation_name = None;
        self.maybe_sender_name = None;
      },
    }
    self
  }
}

/// Stores the session a notification process may use (see [`PushSession`]).
pub fn save_push_session(store: &impl LocalStore, session: &PushSession) -> Result<(), CoreError> {
  records::put_json(store, Table::Account, PUSH_SESSION_RECORD, session)
}

/// The stored session if its token is still valid at `now_ms`.
pub fn push_session(store: &impl LocalStore, now_ms: i64) -> Result<Option<PushSession>, CoreError> {
  Ok(
    records::get_json::<PushSession>(store, Table::Account, PUSH_SESSION_RECORD)?
      .filter(|session| session.expires_at_ms > now_ms),
  )
}

impl std::fmt::Debug for PushSession {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("PushSession")
      .field("server_url", &self.server_url)
      .field("expires_at_ms", &self.expires_at_ms)
      .finish_non_exhaustive()
  }
}

/// From the store if the app already has the message, else fetched and decrypted; failures only
/// cost the preview. Also whether the sender sent it silently, and whether the message is known to
/// belong to the wake-up's conversation.
async fn preview_content<A: ServerApi>(
  store: &impl LocalStore,
  maybe_api: Option<&A>,
  wake_up: &PushWakeUp,
  message_id: MessageId,
) -> (PreviewContent, bool, bool) {
  let conversation = wake_up.conversation_id;
  let local = store
    .messages(conversation, MessageFilter::All)
    .ok()
    .and_then(|messages| {
      messages
        .into_iter()
        .find(|message| message.maybe_server_id == Some(message_id))
    });
  if let Some(message) = local {
    return (
      content_preview(&message.content),
      is_silent(&message.content),
      true,
    );
  }
  let Some(api) = maybe_api else {
    return (PreviewContent::Unavailable, false, false);
  };
  match fetch_and_decrypt(store, api, wake_up, message_id).await {
    Ok(content) => (content_preview(&content), is_silent(&content), true),
    Err(error) => {
      tracing::debug!(%error, conversation_id = %conversation, "no preview for push");
      (PreviewContent::Unavailable, false, false)
    },
  }
}

fn is_silent(content: &MessageContent) -> bool {
  matches!(content, MessageContent::Text { meta, .. } if meta.is_silent)
}

async fn fetch_and_decrypt<A: ServerApi>(
  store: &impl LocalStore,
  api: &A,
  wake_up: &PushWakeUp,
  message_id: MessageId,
) -> Result<MessageContent, CoreError> {
  // `before` is exclusive: start right after the message, newest first.
  let next = Snowflake::new(message_id.snowflake().get().saturating_add(1))
    .map_err(|_| CoreError::Corrupt("message ID out of range".into()))?;
  let query = ListMessagesQuery {
    maybe_before: Some(MessageId::from_snowflake(next)),
    maybe_after: None,
    maybe_limit: Some(1),
  };
  let page = api.list_messages(wake_up.conversation_id, &query).await?;
  let message = page
    .messages
    .into_iter()
    .find(|message| message.id == message_id)
    .ok_or_else(|| CoreError::Corrupt("pushed message not found".into()))?;
  // The key ring follows the message's own conversation: a message of another conversation
  // would decrypt fine and be shown (and answered) as if it belonged to this one.
  if message.conversation_id != wake_up.conversation_id {
    return Err(CoreError::Corrupt(
      "pushed message is from another conversation".into(),
    ));
  }
  // Like the app: nothing a device signed while it was out of the conversation (security review
  // A10, e2ee.md §4.4).
  let key = format!(
    "{}/{}/{}",
    message.conversation_id, message.sender_user_id, message.sender_device_id
  );
  if records::removed_sender(store, &key)?.is_some_and(|absence| {
    absence.covers(
      message.id.snowflake().unix_millis(),
      crate::client::REMOVED_SENDER_GRACE_MS,
    )
  }) {
    return Err(CoreError::Corrupt(
      "pushed message is from a device that had left the conversation".into(),
    ));
  }
  let account = records::load_account(store)?.ok_or(CoreError::NotEnrolled)?;
  let maybe_sender_key =
    records::device_signing_key(store, message.sender_user_id, message.sender_device_id)?;
  // Previews never show unsigned messages.
  decrypt_content(&account.key_bag, &message, maybe_sender_key.as_ref(), false)
}

fn content_preview(content: &MessageContent) -> PreviewContent {
  match content {
    MessageContent::Text { body, .. } => PreviewContent::Text(truncate(body)),
    MessageContent::File { attachment } => PreviewContent::File {
      file_name: attachment.file_name.clone(),
    },
    _ => PreviewContent::Unavailable,
  }
}

fn conversation_name(conversation: &ConversationResponse) -> Option<String> {
  if let Some(name) = &conversation.maybe_name {
    return Some(name.clone());
  }
  let names: Vec<&str> = conversation
    .participants
    .iter()
    .map(|user| user.display_name.as_str())
    .collect();
  (!names.is_empty()).then(|| names.join(", "))
}

fn participant_name(
  maybe_conversation: Option<&ConversationResponse>,
  user: teamcraft_proto::UserId,
) -> Option<String> {
  maybe_conversation?
    .participants
    .iter()
    .find(|participant| participant.id == user)
    .map(|participant| participant.display_name.clone())
}

fn truncate(text: &str) -> String {
  match text.char_indices().nth(MAX_PREVIEW_CHARS) {
    Some((cut, _)) => format!("{}…", &text[..cut]),
    None => text.to_string(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_proto::ConversationId;

  #[test]
  fn long_text_is_cut_at_a_character_boundary() {
    let long = "ä".repeat(MAX_PREVIEW_CHARS + 5);
    let cut = truncate(&long);
    assert_eq!(cut.chars().count(), MAX_PREVIEW_CHARS + 1);
    assert!(cut.ends_with('…'));
    assert_eq!(truncate("short"), "short");
  }

  #[test]
  fn a_missing_or_unreadable_policy_fails_closed() {
    let store = crate::store::MemoryStore::new();
    assert_eq!(push_content_mode(&store), PushContentMode::Generic);
    records::put_json(
      &store,
      Table::Workspace,
      crate::client::WORKSPACE_KEY,
      &serde_json::json!({"policy": "not a policy"}),
    )
    .unwrap();
    assert_eq!(push_content_mode(&store), PushContentMode::Generic);

    let mut state = crate::workspace::WorkspaceState::default();
    state.policy.notifications.push_content = PushContentMode::Sender;
    records::put_json(&store, Table::Workspace, crate::client::WORKSPACE_KEY, &state).unwrap();
    assert_eq!(push_content_mode(&store), PushContentMode::Sender);
  }

  #[test]
  fn the_policy_mode_cuts_the_preview_down() {
    let full = PushPreview {
      wake_up: PushWakeUp {
        kind: teamcraft_proto::PushKind::Message,
        conversation_id: ConversationId::from_snowflake(Snowflake::new(1).unwrap()),
        maybe_message_id: None,
        maybe_sender_id: None,
        is_urgent: false,
        maybe_thread_id: None,
        maybe_call_id: None,
        maybe_badge: None,
        maybe_sent_at: None,
      },
      maybe_conversation_name: Some("planning".into()),
      maybe_sender_name: Some("Alice".into()),
      content: PreviewContent::Text("lunch?".into()),
      is_direct: false,
      is_silent: false,
      is_conversation_verified: true,
    };
    assert_eq!(full.clone().limited_to(&PushContentMode::Full), full);
    let sender = full.clone().limited_to(&PushContentMode::Sender);
    assert_eq!(sender.content, PreviewContent::Unavailable);
    assert_eq!(sender.maybe_sender_name.as_deref(), Some("Alice"));
    for mode in [PushContentMode::Generic, PushContentMode::Unknown("x".into())] {
      let generic = full.clone().limited_to(&mode);
      assert!(generic.maybe_sender_name.is_none() && generic.maybe_conversation_name.is_none());
      assert_eq!(generic.content, PreviewContent::Unavailable);
      assert!(generic.is_conversation_verified);
    }
  }

  #[test]
  fn push_session_is_redacted_and_expires() {
    let store = crate::store::MemoryStore::new();
    let session = PushSession {
      server_url: "https://chat.example.com".into(),
      access_token: "secret-token".into(),
      expires_at_ms: 1_000,
    };
    assert!(!format!("{session:?}").contains("secret-token"));
    save_push_session(&store, &session).unwrap();
    assert_eq!(push_session(&store, 999).unwrap(), Some(session));
    assert_eq!(push_session(&store, 1_000).unwrap(), None);
  }
}
