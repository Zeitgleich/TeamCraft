//! What a notification already told this device about a conversation (issue #1065).
//!
//! A push names the conversation and its sender and, read with the push key, carries the message.
//! Opening that conversation from the notification must show this at once, also when the sync has
//! not brought the conversation or the message yet (a slow link, a channel new to the device, a long
//! backlog). The preview only reads the store and never stores the message itself (only the sync
//! does, in order, so its cursors stay right), so the notification's view is kept here, one record
//! per conversation, until the store has caught up past it.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, MessageId, PushKind, ThreadId, UserId};

use crate::error::CoreError;
use crate::push_preview::{PreviewContent, PushPreview};
use crate::records;
use crate::store::{LocalStore, MessageEdge, Table};

const NOTIFIED_RECORD_PREFIX: &str = "notified/";

/// What the newest notification of a conversation showed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifiedConversation {
  pub conversation_id: ConversationId,
  /// Channel name, or the other participants of a DM.
  pub maybe_name: Option<String>,
  pub is_direct: bool,
  /// The notified message, while the store doesn't have it yet.
  pub maybe_message: Option<NotifiedMessage>,
}

/// The message a notification showed, as far as it could be read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifiedMessage {
  pub message_id: MessageId,
  pub maybe_sender_id: Option<UserId>,
  pub maybe_sender_name: Option<String>,
  pub content: NotifiedContent,
  /// The thread it replies in: then it is not part of the conversation's own feed.
  pub maybe_thread_id: Option<ThreadId>,
}

/// What of the notified message can be shown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NotifiedContent {
  Text {
    text: String,
  },
  File {
    file_name: String,
  },
  /// Not readable here yet (or withheld by the notification settings).
  Unavailable,
}

/// Remembers what `preview` showed of a message, unless an equally new or newer notification of
/// the same conversation is kept already (a retry or a late push). Rings and withdrawals keep
/// nothing, and neither does a preview that names nothing.
pub fn remember(store: &impl LocalStore, preview: &PushPreview) -> Result<(), CoreError> {
  let wake_up = &preview.wake_up;
  let (PushKind::Message | PushKind::Mention, Some(message_id)) = (&wake_up.kind, wake_up.maybe_message_id)
  else {
    return Ok(());
  };
  let content = NotifiedContent::from(&preview.content);
  if preview.maybe_conversation_name.is_none()
    && preview.maybe_sender_name.is_none()
    && content == NotifiedContent::Unavailable
  {
    return Ok(());
  }
  let key = record_key(wake_up.conversation_id);
  let maybe_kept: Option<NotifiedConversation> = records::get_json(store, Table::Account, &key)?;
  let maybe_kept_message = maybe_kept.as_ref().and_then(|kept| kept.maybe_message.as_ref());
  if maybe_kept_message.is_some_and(|kept| kept.message_id >= message_id) {
    return Ok(());
  }
  let notified = NotifiedConversation {
    conversation_id: wake_up.conversation_id,
    // A retry without the name keeps the one an earlier notification knew.
    maybe_name: preview
      .maybe_conversation_name
      .clone()
      .or_else(|| maybe_kept.and_then(|kept| kept.maybe_name)),
    is_direct: preview.is_direct,
    maybe_message: Some(NotifiedMessage {
      message_id,
      maybe_sender_id: wake_up.maybe_sender_id,
      maybe_sender_name: preview.maybe_sender_name.clone(),
      content,
      maybe_thread_id: wake_up.maybe_thread_id,
    }),
  };
  records::put_json(store, Table::Account, &key, &notified)
}

/// What the newest notification showed of `conversation`, for opening it before the sync brought
/// it. The message is left out once the store has caught up to it (it is shown from there, or
/// was deleted meanwhile); the record is dropped once the store knows the conversation too.
pub fn notified_conversation(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Option<NotifiedConversation>, CoreError> {
  let key = record_key(conversation);
  let Some(mut notified) = records::get_json::<NotifiedConversation>(store, Table::Account, &key)? else {
    return Ok(None);
  };
  let maybe_newest = store.edge_server_id(conversation, MessageEdge::Newest)?;
  if let (Some(message), Some(newest)) = (&notified.maybe_message, maybe_newest)
    && newest >= message.message_id
  {
    notified.maybe_message = None;
  }
  let is_conversation_stored = store
    .get(Table::Conversations, &conversation.to_string())?
    .is_some();
  if notified.maybe_message.is_none() && is_conversation_stored {
    store.delete(Table::Account, &key)?;
    return Ok(None);
  }
  Ok(Some(notified))
}

fn record_key(conversation: ConversationId) -> String {
  format!("{NOTIFIED_RECORD_PREFIX}{conversation}")
}

impl From<&PreviewContent> for NotifiedContent {
  fn from(content: &PreviewContent) -> Self {
    match content {
      PreviewContent::Text(text) => Self::Text { text: text.clone() },
      PreviewContent::File { file_name, .. } => Self::File {
        file_name: file_name.clone(),
      },
      PreviewContent::Unavailable => Self::Unavailable,
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ClientMessageId, Id, PushWakeUp, Snowflake};

  use super::*;
  use crate::store::{MemoryStore, MessageContent, StoredMessage};

  const CONVERSATION: i64 = 10;

  #[test]
  fn a_notified_message_is_kept_until_the_store_has_it() {
    let store = MemoryStore::new();
    remember(&store, &preview(5, "lunch?")).unwrap();
    let notified = notified_conversation(&store, conversation()).unwrap().unwrap();
    assert_eq!(notified.maybe_name.as_deref(), Some("planning"));
    let message = notified.maybe_message.unwrap();
    assert_eq!(message.message_id, message_id(5));
    assert_eq!(message.maybe_sender_name.as_deref(), Some("Alice"));
    assert_eq!(
      message.content,
      NotifiedContent::Text {
        text: "lunch?".into()
      }
    );

    // The sync stored the conversation and caught up past the message: nothing left to add.
    store_conversation(&store);
    store.upsert_message(&stored_message(5)).unwrap();
    assert_eq!(notified_conversation(&store, conversation()).unwrap(), None);
    assert!(
      store
        .get(Table::Account, &record_key(conversation()))
        .unwrap()
        .is_none()
    );
  }

  #[test]
  fn the_message_is_dropped_once_the_store_passed_it_even_without_the_conversation() {
    let store = MemoryStore::new();
    remember(&store, &preview(5, "lunch?")).unwrap();
    store.upsert_message(&stored_message(6)).unwrap();
    let notified = notified_conversation(&store, conversation()).unwrap().unwrap();
    assert_eq!(notified.maybe_message, None);
    assert_eq!(notified.maybe_name.as_deref(), Some("planning"));
  }

  #[test]
  fn an_older_or_repeated_notification_never_replaces_a_newer_one() {
    let store = MemoryStore::new();
    remember(&store, &preview(7, "newer")).unwrap();
    remember(&store, &preview(6, "older")).unwrap();
    let mut retry = preview(7, "x");
    retry.content = PreviewContent::Unavailable;
    remember(&store, &retry).unwrap();
    let message = notified_conversation(&store, conversation())
      .unwrap()
      .unwrap()
      .maybe_message
      .unwrap();
    assert_eq!(message.content, NotifiedContent::Text { text: "newer".into() });
  }

  #[test]
  fn a_later_notification_without_the_name_keeps_the_known_one() {
    let store = MemoryStore::new();
    remember(&store, &preview(5, "first")).unwrap();
    let mut later = preview(6, "second");
    later.maybe_conversation_name = None;
    remember(&store, &later).unwrap();
    let notified = notified_conversation(&store, conversation()).unwrap().unwrap();
    assert_eq!(notified.maybe_name.as_deref(), Some("planning"));
    assert_eq!(notified.maybe_message.unwrap().message_id, message_id(6));
  }

  #[test]
  fn rings_withdrawals_and_empty_previews_keep_nothing() {
    let store = MemoryStore::new();
    let mut ring = preview(5, "x");
    ring.wake_up.kind = PushKind::Call;
    remember(&store, &ring).unwrap();
    let mut dismiss = preview(5, "x");
    dismiss.wake_up.kind = PushKind::Dismiss;
    remember(&store, &dismiss).unwrap();
    let mut generic = preview(5, "x");
    generic.maybe_conversation_name = None;
    generic.maybe_sender_name = None;
    generic.content = PreviewContent::Unavailable;
    remember(&store, &generic).unwrap();
    assert_eq!(notified_conversation(&store, conversation()).unwrap(), None);
  }

  fn preview(message: i64, text: &str) -> PushPreview {
    PushPreview {
      wake_up: PushWakeUp {
        kind: PushKind::Message,
        conversation_id: conversation(),
        maybe_message_id: Some(message_id(message)),
        maybe_sender_id: Some(UserId::from_snowflake(Snowflake::new(3).unwrap())),
        is_urgent: false,
        maybe_thread_id: None,
        maybe_call_id: None,
        maybe_badge: None,
        maybe_sent_at: None,
      },
      maybe_conversation_name: Some("planning".into()),
      maybe_sender_name: Some("Alice".into()),
      content: PreviewContent::Text(text.into()),
      is_direct: false,
      is_silent: false,
      is_conversation_verified: true,
      maybe_gap: None,
    }
  }

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(CONVERSATION).unwrap())
  }

  fn message_id(id: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn store_conversation(store: &MemoryStore) {
    store
      .put(Table::Conversations, &conversation().to_string(), b"{}")
      .unwrap();
  }

  fn stored_message(id: i64) -> StoredMessage {
    StoredMessage {
      conversation_id: conversation(),
      client_message_id: ClientMessageId::from_bytes([id as u8; 16]),
      maybe_server_id: Some(message_id(id)),
      sender_user_id: UserId::from_snowflake(Snowflake::new(3).unwrap()),
      maybe_thread_id: None,
      sent_at_ms: 0,
      content: MessageContent::Text {
        body: "hello".into(),
        mentions: Vec::new(),
        meta: Default::default(),
      },
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
    }
  }
}
