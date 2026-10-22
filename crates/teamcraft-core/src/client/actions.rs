//! Message actions beyond plain text: rich sending (replies, priority, acknowledgement requests,
//! silent, "also send to channel"), pins, forwards, acknowledgements and marking unread.

use teamcraft_proto::{ClientMessageId, ConversationId, MessageId, ThreadId, UserId};

use super::Client;
use super::messaging::apply_control;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::read_state;
use crate::store::{Forwarded, LocalStore, MessageContent, MessageMeta, ReplyRef, StoredMessage};

/// Characters of a quoted message kept in a reply (the rest is in the original).
pub const REPLY_EXCERPT_CHARS: usize = 140;

/// A text message with everything the composer can set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutgoingText {
  pub maybe_thread: Option<ThreadId>,
  pub body: String,
  pub mentions: Vec<UserId>,
  pub meta: MessageMeta,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Sends a text with metadata; returns its client message ID.
  pub async fn send(
    &mut self,
    conversation: ConversationId,
    text: OutgoingText,
  ) -> Result<ClientMessageId, CoreError> {
    let body = text.body.clone();
    let content = MessageContent::Text {
      body: text.body,
      mentions: text.mentions,
      meta: text.meta,
    };
    let id = self.enqueue(conversation, text.maybe_thread, content)?;
    let policy = self.workspace_policy()?.link_previews;
    if super::previews::queue_preview(&self.store, &policy, conversation, id, &body)? {
      self.generate_pending_previews().await?;
    }
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Pins or unpins a message for everyone: an encrypted pin message for the members, and the
  /// server's flag so pinned messages survive retention.
  pub async fn set_pinned(
    &mut self,
    conversation: ConversationId,
    target: ClientMessageId,
    is_pinned: bool,
  ) -> Result<(), CoreError> {
    let content = MessageContent::Pin { target, is_pinned };
    self.enqueue(conversation, None, content.clone())?;
    apply_control(
      &self.store,
      conversation,
      self.account.user_id,
      &content,
      self.now_ms(),
    )?;
    self.flush_outbox().await?;
    self.set_server_pin(conversation, target, is_pinned).await
  }

  pub(crate) async fn set_server_pin(
    &mut self,
    conversation: ConversationId,
    target: ClientMessageId,
    is_pinned: bool,
  ) -> Result<(), CoreError> {
    let maybe_server_id = self
      .store
      .message(conversation, target)?
      .and_then(|stored| stored.maybe_server_id);
    match maybe_server_id {
      Some(message) => Ok(
        self
          .api
          .set_message_pinned(conversation, message, is_pinned)
          .await?,
      ),
      // Not sent yet: the encrypted pin still works; the retention flag follows on the next pin.
      None => Ok(()),
    }
  }

  /// Sends a copy of a message to another conversation with an optional comment. The copy is a new
  /// message, encrypted for the target; files go along (their keys travel inside the message).
  pub async fn forward(
    &mut self,
    from: ConversationId,
    target: ClientMessageId,
    to: ConversationId,
    comment: &str,
  ) -> Result<ClientMessageId, CoreError> {
    let original = self
      .store
      .message(from, target)?
      .ok_or_else(|| CoreError::Corrupt("forwarding a message that isn't stored".into()))?;
    let author_name = self.display_name(original.sender_user_id)?;
    let maybe_conversation_name = self.conversation_name(from)?;
    let content = forward_content(&original, author_name, maybe_conversation_name, comment)
      .ok_or_else(|| CoreError::Corrupt("this kind of message can't be forwarded".into()))?;
    let id = self.enqueue(to, None, content)?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Confirms having read a message that asked for it; the sender sees it.
  pub async fn acknowledge(
    &mut self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Result<(), CoreError> {
    self.api.acknowledge_message(conversation, message).await?;
    read_state::note_acknowledgement(&self.store, conversation, message, self.account.user_id)
  }

  /// Marks the conversation unread from `from` on (synced to the user's devices).
  pub fn mark_unread(&mut self, conversation: ConversationId, from: MessageId) -> Result<(), CoreError> {
    self.update_account_data(|data, now| {
      data.mark_unread(conversation, from, now);
      true
    })
  }

  fn display_name(&self, user: UserId) -> Result<String, CoreError> {
    Ok(
      crate::records::get_json::<teamcraft_proto::UserSummary>(
        &self.store,
        crate::store::Table::Users,
        &user.to_string(),
      )?
      .map(|summary| summary.display_name)
      .unwrap_or_default(),
    )
  }

  fn conversation_name(&self, conversation: ConversationId) -> Result<Option<String>, CoreError> {
    Ok(
      crate::records::get_json::<teamcraft_proto::ConversationResponse>(
        &self.store,
        crate::store::Table::Conversations,
        &conversation.to_string(),
      )?
      .and_then(|response| response.maybe_name),
    )
  }
}

/// The reference a reply carries to the quoted message.
pub fn reply_ref(target: &StoredMessage) -> ReplyRef {
  let text = match &target.content {
    MessageContent::Text { body, .. } => body.as_str(),
    MessageContent::File { attachment } => attachment.file_name.as_str(),
    _ => "",
  };
  ReplyRef {
    target: target.client_message_id,
    author: target.sender_user_id,
    excerpt: text.chars().take(REPLY_EXCERPT_CHARS).collect(),
  }
}

/// The content of a forward of `original`: the comment as text, the original quoted with its
/// author, its files attached again. `None` for messages that can't be forwarded (deleted ones,
/// reactions and other control messages).
pub fn forward_content(
  original: &StoredMessage,
  author_name: String,
  maybe_conversation_name: Option<String>,
  comment: &str,
) -> Option<MessageContent> {
  let (body, attachments) = match &original.content {
    MessageContent::Text { body, meta, .. } => (body.clone(), meta.attachments.clone()),
    MessageContent::File { attachment } => (String::new(), vec![attachment.clone()]),
    _ => return None,
  };
  Some(MessageContent::Text {
    body: comment.to_string(),
    mentions: Vec::new(),
    meta: MessageMeta {
      maybe_forwarded: Some(Forwarded {
        author_name,
        maybe_conversation_name,
        sent_at_ms: original.sent_at_ms,
        body,
      }),
      attachments,
      ..MessageMeta::default()
    },
  })
}
