//! Messages: the outbox (client message IDs, retries), encryption with the conversation's content
//! key, decryption of incoming messages and paged history (e2ee.md §4.1, §4.4).

use rand_core::OsRng;
use teamcraft_crypto::message::{
  EncryptedMessage, MessageContext, MessageError, decrypt_message, encrypt_message,
};
use teamcraft_proto::{
  Base64Bytes, ClientMessageId, ConversationId, Id, ListMessagesQuery, MessageResponse, SendMessageRequest,
  ThreadId, UserId,
};
use zeroize::Zeroizing;

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records::{self, OutboxEntry};
use crate::store::{LocalStore, MessageContent, MessageFilter, StoredMessage, Table};

/// Result of [`Client::flush_outbox`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushReport {
  pub sent: usize,
  /// Still waiting: offline, or no content key yet.
  pub pending: usize,
  /// Rejected by the server; kept with the error for the UI to offer "retry" or "delete".
  pub failed: usize,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Writes a text message to the store and the outbox, then tries to send it. Works offline: the
  /// message stays in the outbox until [`Self::flush_outbox`] gets it through.
  pub async fn send_text(
    &mut self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    body: &str,
  ) -> Result<ClientMessageId, CoreError> {
    self
      .send_text_mentioning(conversation, maybe_thread, body, Vec::new())
      .await
  }

  /// Like [`Self::send_text`], notifying `mentions` (their IDs are visible to the server).
  pub async fn send_text_mentioning(
    &mut self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    body: &str,
    mentions: Vec<UserId>,
  ) -> Result<ClientMessageId, CoreError> {
    let content = MessageContent::Text {
      body: body.to_string(),
      mentions,
    };
    let id = self.enqueue(conversation, maybe_thread, content)?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Replaces the text of one of the user's own messages.
  pub async fn edit_message(
    &mut self,
    conversation: ConversationId,
    target: ClientMessageId,
    body: &str,
  ) -> Result<ClientMessageId, CoreError> {
    let id = self.enqueue(
      conversation,
      None,
      MessageContent::Edit {
        target,
        body: body.to_string(),
      },
    )?;
    self.apply_control(
      conversation,
      self.account.user_id,
      &MessageContent::Edit {
        target,
        body: body.to_string(),
      },
    )?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Withdraws one of the user's own messages.
  pub async fn delete_message(
    &mut self,
    conversation: ConversationId,
    target: ClientMessageId,
  ) -> Result<ClientMessageId, CoreError> {
    let id = self.enqueue(conversation, None, MessageContent::Delete { target })?;
    self.apply_control(
      conversation,
      self.account.user_id,
      &MessageContent::Delete { target },
    )?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Adds or removes the user's reaction to a message (reactions are encrypted messages too).
  pub async fn react(
    &mut self,
    conversation: ConversationId,
    target: ClientMessageId,
    emoji: &str,
    is_added: bool,
  ) -> Result<ClientMessageId, CoreError> {
    let content = MessageContent::Reaction {
      target,
      emoji: emoji.to_string(),
      is_added,
    };
    let id = self.enqueue(conversation, None, content)?;
    self.flush_outbox().await?;
    Ok(id)
  }

  /// Sends what's in the outbox, oldest first. Stops at the first network error.
  pub async fn flush_outbox(&mut self) -> Result<FlushReport, CoreError> {
    let mut report = FlushReport::default();
    let mut entries = records::outbox(&self.store)?;
    entries.sort_by_key(|entry| {
      self
        .store
        .message(entry.conversation_id, entry.client_message_id)
        .ok()
        .flatten()
        .map_or(0, |m| m.sent_at_ms)
    });
    let mut is_offline = false;
    for mut entry in entries {
      if entry.maybe_last_error.is_some() {
        report.failed += 1;
        continue;
      }
      if is_offline {
        report.pending += 1;
        continue;
      }
      match self.send_entry(&entry).await {
        Ok(true) => report.sent += 1,
        Ok(false) => report.pending += 1,
        Err(error) if error.is_transient() => {
          is_offline = true;
          report.pending += 1;
        },
        Err(error) => {
          tracing::warn!(conversation_id = %entry.conversation_id, %error, "message rejected");
          entry.attempts += 1;
          entry.maybe_last_error = Some(error.to_string());
          let key = records::outbox_key(entry.conversation_id, entry.client_message_id);
          records::put_json(&self.store, Table::Outbox, &key, &entry)?;
          report.failed += 1;
        },
      }
    }
    Ok(report)
  }

  /// Puts a failed outbox message back in line, e.g. after the user tapped "retry".
  pub fn retry_failed(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), CoreError> {
    let key = records::outbox_key(conversation, client_message_id);
    if let Some(mut entry) = records::get_json::<OutboxEntry>(&self.store, Table::Outbox, &key)? {
      entry.maybe_last_error = None;
      records::put_json(&self.store, Table::Outbox, &key, &entry)?;
    }
    Ok(())
  }

  /// Loads one page of older history before the oldest message this device has. Returns how many
  /// messages arrived; `0` means the beginning (or the history this device may see) is reached.
  pub async fn load_older_messages(&mut self, conversation: ConversationId) -> Result<usize, CoreError> {
    let maybe_oldest = self
      .store
      .messages(conversation, MessageFilter::All)?
      .into_iter()
      .find_map(|message| message.maybe_server_id);
    let query = ListMessagesQuery {
      maybe_before: maybe_oldest,
      maybe_after: None,
      maybe_limit: Some(self.settings.history_page_size),
    };
    let page = self.api.list_messages(conversation, &query).await?;
    let count = page.messages.len();
    for message in page.messages {
      self.receive_message(message)?;
    }
    Ok(count)
  }

  /// Fetches messages newer than the newest one this device has (after being offline).
  pub async fn catch_up_messages(&mut self, conversation: ConversationId) -> Result<usize, CoreError> {
    let maybe_newest = self
      .store
      .messages(conversation, MessageFilter::All)?
      .into_iter()
      .filter_map(|message| message.maybe_server_id)
      .max();
    let Some(newest) = maybe_newest else {
      return self.load_older_messages(conversation).await;
    };
    let mut total = 0;
    let mut cursor = newest;
    loop {
      let query = ListMessagesQuery {
        maybe_before: None,
        maybe_after: Some(cursor),
        maybe_limit: Some(self.settings.history_page_size),
      };
      let page = self.api.list_messages(conversation, &query).await?;
      total += page.messages.len();
      for message in page.messages {
        cursor = cursor.max(message.id);
        self.receive_message(message)?;
      }
      if !page.has_more {
        return Ok(total);
      }
    }
  }

  /// Decrypts and stores a message from the server. Messages whose content key hasn't arrived yet
  /// are parked and retried when keys arrive.
  pub(crate) fn receive_message(&mut self, message: MessageResponse) -> Result<(), CoreError> {
    let conversation = message.conversation_id;
    if let Some(existing) = self.store.message(conversation, message.client_message_id)?
      && existing.maybe_server_id.is_some()
    {
      // Already known (own message echoed back, or a duplicate delivery).
      return Ok(());
    }
    match self.decrypt(&message) {
      Ok(content) => {
        self.store.upsert_message(&StoredMessage {
          conversation_id: conversation,
          client_message_id: message.client_message_id,
          maybe_server_id: Some(message.id),
          sender_user_id: message.sender_user_id,
          maybe_thread_id: message.maybe_thread_id,
          sent_at_ms: message.id.snowflake().unix_millis(),
          content: content.clone(),
          is_edited: false,
        })?;
        self.apply_control(conversation, message.sender_user_id, &content)?;
        self
          .store
          .delete(Table::Undecrypted, &undecrypted_key(&message))?;
        Ok(())
      },
      Err(CoreError::Message(MessageError::UnknownKey(_))) | Err(CoreError::NoContentKey(_)) => {
        records::put_json(
          &self.store,
          Table::Undecrypted,
          &undecrypted_key(&message),
          &message,
        )
      },
      Err(error) => {
        // Tampered or wrongly attributed ciphertext: never shown (e2ee.md §4.4).
        tracing::warn!(conversation_id = %conversation, message_id = %message.id, %error, "dropped undecryptable message");
        Ok(())
      },
    }
  }

  /// Applies an edit or delete to its target. Only the target's author may change it; others' edits
  /// and deletes are ignored (moderator deletion is a server-side feature, PLAN.md).
  fn apply_control(
    &self,
    conversation: ConversationId,
    sender: UserId,
    content: &MessageContent,
  ) -> Result<(), CoreError> {
    let (target, replacement) = match content {
      MessageContent::Edit { target, body } => (
        *target,
        MessageContent::Text {
          body: body.clone(),
          mentions: Vec::new(),
        },
      ),
      MessageContent::Delete { target } => (*target, MessageContent::Deleted),
      _ => return Ok(()),
    };
    let Some(mut stored) = self.store.message(conversation, target)? else {
      // TODO(ordering): an edit that arrives before its target is kept but not applied later.
      return Ok(());
    };
    if stored.sender_user_id != sender || matches!(stored.content, MessageContent::Deleted) {
      return Ok(());
    }
    if let (MessageContent::Text { mentions, .. }, MessageContent::Text { body, .. }) =
      (&stored.content, &replacement)
    {
      stored.content = MessageContent::Text {
        body: body.clone(),
        mentions: mentions.clone(),
      };
      stored.is_edited = true;
    } else if matches!(replacement, MessageContent::Deleted) {
      stored.content = MessageContent::Deleted;
    } else {
      return Ok(());
    }
    self.store.upsert_message(&stored)?;
    Ok(())
  }

  /// Tries the parked messages of a conversation again (after new content keys arrived).
  pub(crate) fn retry_undecrypted(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let parked: Vec<MessageResponse> = records::list_json(&self.store, Table::Undecrypted)?;
    for message in parked
      .into_iter()
      .filter(|message| message.conversation_id == conversation)
    {
      self.receive_message(message)?;
    }
    Ok(())
  }

  /// Messages this device received but can't read yet.
  pub fn undecrypted_count(&self) -> Result<usize, CoreError> {
    Ok(self.store.list(Table::Undecrypted)?.len())
  }

  pub(crate) fn enqueue(
    &mut self,
    conversation: ConversationId,
    maybe_thread: Option<ThreadId>,
    content: MessageContent,
  ) -> Result<ClientMessageId, CoreError> {
    let draft = records::Draft {
      conversation_id: conversation,
      maybe_thread_id: maybe_thread,
      content,
    };
    records::enqueue_message(&self.store, self.account.user_id, self.now_ms(), draft)
  }

  /// `Ok(false)`: not sendable yet (no content key).
  async fn send_entry(&mut self, entry: &OutboxEntry) -> Result<bool, CoreError> {
    let key = records::outbox_key(entry.conversation_id, entry.client_message_id);
    let Some(mut stored) = self
      .store
      .message(entry.conversation_id, entry.client_message_id)?
    else {
      self.store.delete(Table::Outbox, &key)?;
      return Ok(true);
    };
    let Some(ring) = self
      .account
      .key_bag
      .ring(convert::conversation(entry.conversation_id))
      .filter(|ring| ring.current().is_some())
    else {
      return Ok(false);
    };
    let plaintext = Zeroizing::new(
      serde_json::to_vec(&stored.content).map_err(|error| CoreError::Corrupt(error.to_string()))?,
    );
    let context = self.message_context(entry.conversation_id, entry.client_message_id);
    let encrypted = encrypt_message(ring, &context, &plaintext, &mut OsRng)?;
    let request = SendMessageRequest {
      client_message_id: entry.client_message_id,
      device_id: self.account.device_id,
      content_key_id: Base64Bytes::new(encrypted.content_key_id().as_bytes().to_vec()),
      ciphertext: Base64Bytes::new(encrypted.to_bytes()?),
      maybe_thread_id: entry.maybe_thread_id,
      mentioned_user_ids: match &stored.content {
        MessageContent::Text { mentions, .. } => mentions.clone(),
        _ => Vec::new(),
      },
      is_urgent: false,
    };
    let response = self.api.send_message(entry.conversation_id, &request).await?;
    stored.maybe_server_id = Some(response.id);
    stored.sent_at_ms = response.id.snowflake().unix_millis();
    self.store.upsert_message(&stored)?;
    self.store.delete(Table::Outbox, &key)?;
    Ok(true)
  }

  fn decrypt(&self, message: &MessageResponse) -> Result<MessageContent, CoreError> {
    let ring = self
      .account
      .key_bag
      .ring(convert::conversation(message.conversation_id))
      .ok_or(CoreError::NoContentKey(message.conversation_id))?;
    let encrypted = EncryptedMessage::from_bytes(message.ciphertext.as_slice())?;
    let context = MessageContext {
      conversation_id: convert::conversation(message.conversation_id),
      sender_user_id: convert::user(message.sender_user_id),
      sender_device_id: convert::device(message.sender_device_id),
      client_message_id: convert::client_message(message.client_message_id),
    };
    let plaintext = decrypt_message(ring, &context, &encrypted)?;
    Ok(serde_json::from_slice(plaintext.expose_secret()).unwrap_or(MessageContent::Unknown))
  }

  fn message_context(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> MessageContext {
    MessageContext {
      conversation_id: convert::conversation(conversation),
      sender_user_id: convert::user(self.account.user_id),
      sender_device_id: convert::device(self.account.device_id),
      client_message_id: convert::client_message(client_message_id),
    }
  }
}

fn undecrypted_key(message: &MessageResponse) -> String {
  format!(
    "{}/{}",
    message.conversation_id,
    message.client_message_id.to_hex()
  )
}
