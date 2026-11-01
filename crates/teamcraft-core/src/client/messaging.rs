//! Messages: the outbox (client message IDs, retries), encryption with the conversation's content
//! key, decryption of incoming messages and paged history (e2ee.md §4.1, §4.4).

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::key_bag::KeyBag;
use teamcraft_crypto::message::{
  EncryptedMessage, MessageContext, MessageError, decrypt_message, decrypt_unsigned_message, encrypt_message,
};
use teamcraft_proto::{
  Base64Bytes, ClientMessageId, ConversationId, Id, ListMessagesQuery, ListMessagesResponse, MessageId,
  MessageResponse, SendMessageRequest, Snowflake, ThreadId, UserId,
};
use zeroize::Zeroizing;

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::convert;
use crate::error::CoreError;
use crate::records::{self, OutboxEntry};
use crate::store::{
  EditRecord, LocalStore, MessageContent, MessageEdge, Priority, StoredMessage, Table, Unreadable,
};

/// Result of [`Client::flush_outbox`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushReport {
  pub sent: usize,
  /// Still waiting: offline, or no content key yet.
  pub pending: usize,
  /// Rejected by the server; kept with the error for the UI to offer "retry" or "delete".
  pub failed: usize,
}

/// Messages a removed device sent shortly before its removal reached this device can carry a
/// slightly later server time; they still count.
pub(crate) const REMOVED_SENDER_GRACE_MS: i64 = 5 * 60 * 1000;

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
      meta: Default::default(),
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
    apply_control(
      &self.store,
      conversation,
      self.account.user_id,
      &MessageContent::Edit {
        target,
        body: body.to_string(),
      },
      self.now_ms(),
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
    apply_control(
      &self.store,
      conversation,
      self.account.user_id,
      &MessageContent::Delete { target },
      self.now_ms(),
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
      if is_offline
        || super::files::pending_upload(&self.store, entry.conversation_id, entry.client_message_id)?
          .is_some()
        || super::previews::has_pending_preview(&self.store, entry.conversation_id, entry.client_message_id)?
      {
        // Offline, its files are still uploading, or its link preview is being made.
        report.pending += 1;
        continue;
      }
      // Never send under a key a member removed meanwhile still holds: catch up the group first
      // (e2ee.md §4.6).
      match self.ensure_mls_current(entry.conversation_id).await {
        Ok(()) => {},
        Err(error) if error.is_transient() => {
          is_offline = true;
          report.pending += 1;
          continue;
        },
        // Not something a retry fixes; holding the message back for good would be worse.
        Err(error) => {
          tracing::warn!(conversation_id = %entry.conversation_id, %error, "sending without MLS catch-up")
        },
      }
      match self.send_entry(&entry).await {
        Ok(true) => report.sent += 1,
        Ok(false) => report.pending += 1,
        Err(error) if error.is_retry_later() => {
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
    records::retry_failed(&self.store, conversation, client_message_id)
  }

  /// Loads one page of older history before the oldest message this device has. Returns how many
  /// messages arrived; `0` means the beginning (or the history this device may see) is reached.
  pub async fn load_older_messages(&mut self, conversation: ConversationId) -> Result<usize, CoreError> {
    let maybe_oldest = self.store.edge_server_id(conversation, MessageEdge::Oldest)?;
    let query = ListMessagesQuery {
      maybe_before: maybe_oldest,
      maybe_after: None,
      maybe_limit: Some(self.settings.history_page_size),
    };
    let page = self.api.list_messages(conversation, &query).await?;
    let count = page.messages.len();
    self.receive_page(page.messages)?;
    Ok(count)
  }

  /// Loads the first page of history sent at or after `from_ms` (Unix milliseconds), for "jump to
  /// date" in channels too big to hold locally. Message IDs are snowflakes, so the time is a cursor
  /// and no scan is needed. The page is merged into the store next to what is already there; the
  /// messages between this window and the newest ones stay unloaded until the user scrolls to them.
  /// Returns how many messages arrived.
  pub async fn load_messages_from(
    &mut self,
    conversation: ConversationId,
    from_ms: i64,
  ) -> Result<usize, CoreError> {
    let query = ListMessagesQuery {
      maybe_before: None,
      maybe_after: Some(MessageId::from_snowflake(Snowflake::cursor_before(from_ms))),
      maybe_limit: Some(self.settings.history_page_size),
    };
    let page = self.api.list_messages(conversation, &query).await?;
    let count = page.messages.len();
    self.receive_page(page.messages)?;
    Ok(count)
  }

  /// Fetches messages newer than the newest one this device has (after being offline), and the
  /// first time the newest page of history.
  pub async fn catch_up_messages(&mut self, conversation: ConversationId) -> Result<usize, CoreError> {
    self.catch_up_messages_from(conversation, None).await
  }

  /// [`Self::catch_up_messages`] with its first request already answered (a catch-up reads
  /// several conversations' first pages at once, issue #247): `maybe_first` answers
  /// [`Self::first_messages_query`].
  pub(crate) async fn catch_up_messages_from(
    &mut self,
    conversation: ConversationId,
    mut maybe_first: Option<Result<ListMessagesResponse, ApiError>>,
  ) -> Result<usize, CoreError> {
    let mut total = 0;
    if !self.is_history_loaded(conversation)? {
      let query = self.first_messages_query(conversation)?;
      let page = match maybe_first.take() {
        Some(first) => first?,
        None => self.api.list_messages(conversation, &query).await?,
      };
      total += page.messages.len();
      self.receive_page(page.messages)?;
      records::put_json(&self.store, Table::SyncState, &history_key(conversation), &true)?;
      if query.maybe_before.is_none() {
        // That was the newest page: what came since arrives as gateway events.
        return Ok(total);
      }
    }
    // Without a stored server message (an empty conversation, or only own messages not sent yet)
    // there is no cursor: the newest page is read, and what came since arrives as events.
    let mut maybe_cursor = self.store.edge_server_id(conversation, MessageEdge::Newest)?;
    loop {
      let is_newest_page = maybe_cursor.is_none();
      let page = match maybe_first.take() {
        Some(first) => first?,
        None => {
          let query = ListMessagesQuery {
            maybe_before: None,
            maybe_after: maybe_cursor,
            maybe_limit: Some(self.settings.history_page_size),
          };
          self.api.list_messages(conversation, &query).await?
        },
      };
      total += page.messages.len();
      maybe_cursor = maybe_cursor.max(page.messages.iter().map(|message| message.id).max());
      self.receive_page(page.messages)?;
      // The newest page's `has_more` speaks of older messages, not newer ones.
      if is_newest_page || !page.has_more {
        return Ok(total + self.load_imported_history(conversation).await?);
      }
    }
  }

  /// Loads the history of an import that just finished, which came without message events: the
  /// conversation's first page if this device hasn't read it yet, else a page of the import.
  pub(super) async fn load_finished_import(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.unloaded_import(conversation)?.is_some() {
      self.catch_up_messages(conversation).await?;
    }
    Ok(())
  }

  /// History imported into a conversation this device already held (e2ee.md §4.10) lies before
  /// everything it has. Once per finished import a page of it is loaded at once, so it shows
  /// right away; the rest follows in the background ([`Self::page_imported_history`]).
  async fn load_imported_history(&mut self, conversation: ConversationId) -> Result<usize, CoreError> {
    let Some(through) = self.unloaded_import(conversation)? else {
      return Ok(0);
    };
    self.is_imported_history_paged = false;
    let is_held = self
      .store
      .edge_server_id(conversation, MessageEdge::Oldest)?
      .is_some_and(|oldest| oldest <= through);
    let loaded = if is_held {
      0
    } else {
      self.load_older_messages(conversation).await?
    };
    records::put_json(
      &self.store,
      Table::SyncState,
      &imported_key(conversation),
      &through,
    )?;
    Ok(loaded)
  }

  /// Where a finished import of `conversation` ends, if this device hasn't loaded it yet. The
  /// import doesn't move the conversation's newest message, so a sync that compares heads reads
  /// the conversation again for this alone.
  pub(super) fn unloaded_import(&self, conversation: ConversationId) -> Result<Option<MessageId>, CoreError> {
    let Some(through) = records::get_json::<teamcraft_proto::ConversationResponse>(
      &self.store,
      Table::Conversations,
      &conversation.to_string(),
    )?
    .and_then(|response| response.maybe_imported_through) else {
      return Ok(None);
    };
    let loaded = records::get_json::<MessageId>(&self.store, Table::SyncState, &imported_key(conversation))?;
    Ok((loaded != Some(through)).then_some(through))
  }

  /// Whether the first page of history was loaded (the first catch-up of a conversation).
  pub(crate) fn is_history_loaded(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(records::get_json::<bool>(&self.store, Table::SyncState, &history_key(conversation))?.is_some())
  }

  /// [`Self::receive_message`] for a page of messages, as one group of writes: a message is
  /// several writes (the message, its effects, the parked copy's removal), and on SQLCipher each
  /// write on its own is a commit with its syncs; a catch-up after a long absence read pages of
  /// them one commit at a time (issue #972). Receiving is idempotent, so a crash that drops the
  /// group only means the page is read again.
  pub(super) fn receive_page(&mut self, messages: Vec<MessageResponse>) -> Result<(), CoreError> {
    self.atomically(|client| {
      for message in messages {
        client.receive_message(message)?;
      }
      Ok(())
    })
  }

  /// Decrypts and stores a message from the server. Messages whose content key hasn't arrived yet
  /// are parked and retried when keys arrive.
  pub(crate) fn receive_message(&mut self, message: MessageResponse) -> Result<(), CoreError> {
    let conversation = message.conversation_id;
    let maybe_existing = self.store.message(conversation, message.client_message_id)?;
    if let Some(existing) = &maybe_existing
      && existing.maybe_unreadable.is_none()
      && (existing.maybe_server_id.is_some() || existing.is_local_line())
    {
      // Already known (own message echoed back, or a duplicate delivery), or the ID of a line this
      // device derived, which no message replaces (issues #811, #818).
      return Ok(());
    }
    if self.sent_after_leaving(&message)? {
      // Signed by a device that had left the conversation when this was sent (security review A10).
      tracing::warn!(conversation_id = %conversation, message_id = %message.id, "dropped a message from a removed device");
      return Ok(());
    }
    match self.decrypt(&message) {
      Ok(mut content) => {
        if let MessageContent::Text { meta, .. } = &mut content {
          if let Some(head) = meta.maybe_workspace_head.take() {
            self.note_workspace_head(&head)?;
          }
          if let Some(head) = meta.maybe_channel_roles_head.take() {
            self.note_channel_roles_head(conversation, &head)?;
          }
        }
        let stored = StoredMessage {
          conversation_id: conversation,
          client_message_id: message.client_message_id,
          maybe_server_id: Some(message.id),
          sender_user_id: message.sender_user_id,
          maybe_thread_id: message.maybe_thread_id,
          sent_at_ms: message.id.snowflake().unix_millis(),
          content: content.clone(),
          is_edited: false,
          edits: Vec::new(),
          maybe_edit_id: None,
          is_pinned: false,
          maybe_verified_commit: None,
          maybe_server_change: None,
          maybe_unreadable: None,
        };
        if super::server_changes::is_announcement(&stored) {
          // An announcement of a change the server reports and the name it puts on a line reach
          // the disk together: after a crash in between, the message would be known and the line
          // would never get the name (issue #818).
          crate::store::write_group(&self.store, || -> Result<(), CoreError> {
            self.store.upsert_message(&stored)?;
            super::server_changes::note_announcement(
              &self.store,
              &stored,
              self.settings.change_announcement_window,
            )
          })?;
        } else {
          self.store.upsert_message(&stored)?;
        }
        apply_control_with(
          &self.store,
          conversation,
          message.sender_user_id,
          &content,
          message.id.snowflake().unix_millis(),
          Some(message.id),
        )?;
        apply_early_controls(&self.store, conversation, message.client_message_id)?;
        self
          .store
          .delete(Table::Undecrypted, &undecrypted_key(&message))?;
        Ok(())
      },
      Err(CoreError::Message(MessageError::UnknownKey(_)))
      | Err(CoreError::NoContentKey(_))
      | Err(CoreError::UnknownSender { .. }) => {
        let now = self.now_ms();
        self.park(
          &message,
          maybe_existing.as_ref(),
          Unreadable::Waiting { since_ms: now },
        )
      },
      // From a newer version: kept and tried again (after an update it opens), never a failure.
      Err(CoreError::Message(MessageError::UnsupportedVersion(version))) => {
        tracing::info!(conversation_id = %conversation, message_id = %message.id, version, "message of a newer version kept");
        self.park(&message, maybe_existing.as_ref(), Unreadable::NotAvailable)
      },
      Err(error @ CoreError::Store(_)) => Err(error),
      Err(
        error @ CoreError::Message(
          MessageError::DecryptionFailed | MessageError::SignerMismatch | MessageError::InvalidSignature,
        ),
      ) if is_signed(&message) => {
        // Tampered or wrongly attributed ciphertext under a key this device has: its content is
        // never shown (e2ee.md §4.4), only a placeholder saying it failed, and it isn't tried again.
        tracing::warn!(conversation_id = %conversation, message_id = %message.id, %error, "message failed to decrypt");
        self.note_failed(&message, maybe_existing.as_ref())
      },
      Err(error) => {
        // Malformed, unsigned (refused by the settings) or of another conversation: dropped as
        // before, with no row that would claim more than the server's word.
        tracing::warn!(conversation_id = %conversation, message_id = %message.id, %error, "dropped undecryptable message");
        Ok(())
      },
    }
  }

  /// Keeps a message whose content key (or sender key) hasn't arrived for a retry, with a
  /// placeholder at its place in the conversation (issue #888).
  fn park(
    &mut self,
    message: &MessageResponse,
    maybe_existing: Option<&StoredMessage>,
    state: Unreadable,
  ) -> Result<(), CoreError> {
    let key = undecrypted_key(message);
    let is_parked = self.store.get(Table::Undecrypted, &key)?.is_some();
    // A retry of a message parked already (new keys arrived, but not its own) writes nothing:
    // every epoch change retries all parked messages of the conversation (issue #1000). One
    // parked by a version without placeholders gets its placeholder now.
    if is_parked && maybe_existing.is_some() {
      return Ok(());
    }
    crate::store::write_group(&self.store, || -> Result<(), CoreError> {
      if !is_parked {
        records::put_json(&self.store, Table::Undecrypted, &key, message)?;
      }
      if maybe_existing.is_none() {
        self.store.upsert_message(&placeholder(message, state))?;
        if let Unreadable::Waiting { since_ms } = state {
          super::missing_keys::note_waiting(&self.store, message.conversation_id, since_ms)?;
        }
      }
      Ok(())
    })
  }

  /// Marks a message that can't be decrypted with the key it names: a placeholder that says so
  /// takes its place (or that of a placeholder waiting for its key), and it is no longer retried.
  fn note_failed(
    &mut self,
    message: &MessageResponse,
    maybe_existing: Option<&StoredMessage>,
  ) -> Result<(), CoreError> {
    // Only the message the row and the parked copy stand for: a server re-delivering garbage under
    // a waiting message's client ID must not discard the real ciphertext or mark it failed.
    let key = undecrypted_key(message);
    let maybe_parked: Option<MessageResponse> = records::get_json(&self.store, Table::Undecrypted, &key)?;
    let is_same_parked = maybe_parked.as_ref().is_none_or(|parked| parked.id == message.id);
    let is_same_row = match maybe_existing {
      None => true,
      Some(existing) => existing.maybe_unreadable.is_some() && existing.maybe_server_id == Some(message.id),
    };
    if !is_same_parked || !is_same_row {
      tracing::warn!(conversation_id = %message.conversation_id, message_id = %message.id, "dropped a failing message that reuses another's client ID");
      return Ok(());
    }
    // A bound on rows the server can make by sending garbage (configurable).
    if maybe_existing.is_none()
      && !super::missing_keys::take_failed_slot(
        &self.store,
        message.conversation_id,
        self.settings.max_failed_placeholders,
      )?
    {
      tracing::warn!(conversation_id = %message.conversation_id, message_id = %message.id, "too many failed messages; dropped without a placeholder");
      return Ok(());
    }
    crate::store::write_group(&self.store, || -> Result<(), CoreError> {
      self
        .store
        .upsert_message(&placeholder(message, Unreadable::Failed))?;
      if maybe_parked.is_some() {
        self.store.delete(Table::Undecrypted, &key)?;
      }
      Ok(())
    })
  }

  /// Applies an edit or delete to its target. Only the target's author may change it; others' edits
  /// and deletes are ignored (moderator deletion is a server-side feature, PLAN.md).
  /// Tries the parked messages of a conversation again (after new content keys arrived).
  pub(crate) fn retry_undecrypted(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let parked: Vec<(String, MessageResponse)> =
      records::list_json_with_prefix(&self.store, Table::Undecrypted, &format!("{conversation}/"))?;
    if parked.is_empty() {
      return Ok(());
    }
    self.receive_page(
      parked
        .into_iter()
        .map(|(_, message)| message)
        .filter(|message| message.conversation_id == conversation)
        .collect(),
    )?;
    self.refresh_key_wait(conversation)
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
    let mut content = stored.content.clone();
    if let MessageContent::Text { meta, .. } = &mut content {
      meta.maybe_workspace_head = Some(Box::new(self.workspace()?.head()));
      meta.maybe_channel_roles_head = self.channel_roles_head(entry.conversation_id)?.map(Box::new);
    }
    let plaintext =
      Zeroizing::new(serde_json::to_vec(&content).map_err(|error| CoreError::Corrupt(error.to_string()))?);
    let context = self.message_context(entry.conversation_id, entry.client_message_id);
    let encrypted = encrypt_message(ring, &context, &plaintext, &self.account.device, &mut OsRng)?;
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
      // The one piece of metadata the server sees: it routes urgent pushes past do-not-disturb.
      is_urgent: matches!(&stored.content, MessageContent::Text { meta, .. } if meta.priority == Priority::Urgent),
    };
    let response = self.api.send_message(entry.conversation_id, &request).await?;
    stored.maybe_server_id = Some(response.id);
    stored.sent_at_ms = response.id.snowflake().unix_millis();
    crate::store::write_group(&self.store, || -> Result<(), CoreError> {
      self.store.upsert_message(&stored)?;
      self.store.delete(Table::Outbox, &key)?;
      // An own edit showed at once; with its server ID it now competes with other devices' edits
      // like theirs do (issue #160).
      if matches!(stored.content, MessageContent::Edit { .. }) {
        apply_control_with(
          &self.store,
          entry.conversation_id,
          self.account.user_id,
          &stored.content,
          stored.sent_at_ms,
          Some(response.id),
        )?;
      }
      Ok(())
    })?;
    Ok(true)
  }

  fn decrypt(&self, message: &MessageResponse) -> Result<MessageContent, CoreError> {
    let maybe_sender_key = if message.sender_device_id == self.account.device_id {
      Some(self.account.device.public_keys().signing_key)
    } else {
      records::device_signing_key(&self.store, message.sender_user_id, message.sender_device_id)?
    };
    decrypt_content(
      &self.account.key_bag,
      message,
      maybe_sender_key.as_ref(),
      self.settings.accept_unsigned_messages,
    )
  }

  /// Whether the message's device was out of the conversation at the message's time (it had
  /// left and, if it came back, not yet).
  fn sent_after_leaving(&self, message: &MessageResponse) -> Result<bool, CoreError> {
    let key = format!(
      "{}/{}/{}",
      message.conversation_id, message.sender_user_id, message.sender_device_id
    );
    Ok(
      records::removed_sender(&self.store, &key)?
        .is_some_and(|absence| absence.covers(message.id.snowflake().unix_millis(), REMOVED_SENDER_GRACE_MS)),
    )
  }

  pub(super) fn message_context(
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

/// Whether the message's ciphertext carries a sender signature. An unsigned one refused by the
/// settings is no tampering.
fn is_signed(message: &MessageResponse) -> bool {
  EncryptedMessage::from_bytes(message.ciphertext.as_slice()).is_ok_and(|encrypted| encrypted.is_signed())
}

/// The row that stands for a message this device can't read: only what the server's envelope
/// says (sender, time and thread), never content (issue #888).
fn placeholder(message: &MessageResponse, unreadable: Unreadable) -> StoredMessage {
  StoredMessage {
    conversation_id: message.conversation_id,
    client_message_id: message.client_message_id,
    maybe_server_id: Some(message.id),
    sender_user_id: message.sender_user_id,
    maybe_thread_id: message.maybe_thread_id,
    sent_at_ms: message.id.snowflake().unix_millis(),
    content: MessageContent::Unknown,
    is_edited: false,
    edits: Vec::new(),
    maybe_edit_id: None,
    is_pinned: false,
    maybe_verified_commit: None,
    maybe_server_change: None,
    maybe_unreadable: Some(unreadable),
  }
}

pub(super) fn undecrypted_key(message: &MessageResponse) -> String {
  format!(
    "{}/{}",
    message.conversation_id,
    message.client_message_id.to_hex()
  )
}

/// Decrypts a message with the content keys in `key_bag`, checking the sender device's signature
/// with `maybe_sender_key` (its verified signing key; without one a signed message waits). Unsigned
/// messages from before signatures are opened only with `accept_unsigned`. Unknown content kinds
/// (from newer clients) become [`MessageContent::Unknown`].
pub(crate) fn decrypt_content(
  key_bag: &KeyBag,
  message: &MessageResponse,
  maybe_sender_key: Option<&teamcraft_crypto::identity::DeviceSigningPublicKey>,
  accept_unsigned: bool,
) -> Result<MessageContent, CoreError> {
  let ring = key_bag
    .ring(convert::conversation(message.conversation_id))
    .ok_or(CoreError::NoContentKey(message.conversation_id))?;
  let encrypted = EncryptedMessage::from_bytes(message.ciphertext.as_slice())?;
  let context = MessageContext {
    conversation_id: convert::conversation(message.conversation_id),
    sender_user_id: convert::user(message.sender_user_id),
    sender_device_id: convert::device(message.sender_device_id),
    client_message_id: convert::client_message(message.client_message_id),
  };
  let plaintext = if encrypted.is_signed() {
    let sender_key = maybe_sender_key.ok_or(CoreError::UnknownSender {
      user: message.sender_user_id,
      device: message.sender_device_id,
    })?;
    decrypt_message(ring, &context, &encrypted, sender_key)?
  } else if accept_unsigned {
    decrypt_unsigned_message(ring, &context, &encrypted)?
  } else {
    return Err(CoreError::Message(MessageError::InvalidSignature));
  };
  Ok(serde_json::from_slice(plaintext.expose_secret()).unwrap_or(MessageContent::Unknown))
}

/// Applies a message that acts on another one: an edit (keeping the earlier text as history), a
/// delete or a pin. Edits and deletes count only from the original sender; pins from any member
/// (the server checks who may pin). For an own change not sent yet; received ones go through
/// [`apply_control_with`] with their server ID.
pub(crate) fn apply_control(
  store: &impl LocalStore,
  conversation: ConversationId,
  sender: UserId,
  content: &MessageContent,
  at_ms: i64,
) -> Result<(), CoreError> {
  apply_control_with(store, conversation, sender, content, at_ms, None)
}

/// [`apply_control`] for a control the server numbered (`maybe_server_id`). Edits converge on
/// every device (issue #160): the one with the highest server ID wins, whatever order they arrive
/// in; an own edit without an ID yet shows at once and is settled once it was sent.
fn apply_control_with(
  store: &impl LocalStore,
  conversation: ConversationId,
  sender: UserId,
  content: &MessageContent,
  at_ms: i64,
  maybe_server_id: Option<MessageId>,
) -> Result<(), CoreError> {
  if let MessageContent::Bookmarks { items } = content {
    return records::put_json(store, Table::Account, &bookmarks_key(conversation), items);
  }
  if let MessageContent::IntegrationSource { change } = content {
    // Decrypted in the conversation's group, so `sender` is a member of it; who may do what is
    // decided when the sources are used (integration_sources.rs).
    return crate::integration_sources::apply_source_change(
      store,
      conversation,
      sender,
      change,
      at_ms,
      maybe_server_id,
    );
  }
  if let MessageContent::Notetaker { request } = content {
    // Decrypted in the conversation's group, so `sender` is a member of it; whether the request
    // counts is decided from the verified policy and roster (notetaker.rs).
    let record = crate::notetaker::NotetakerRequestRecord {
      by: sender,
      request: request.clone(),
      at_ms,
      maybe_server_id,
    };
    return crate::notetaker::apply_request(store, conversation, record);
  }
  if let MessageContent::Event {
    event: crate::store::ConversationEvent::CallMoved {
      meeting,
      added,
      maybe_call,
    },
  } = content
  {
    // Decrypted in the conversation's group, so `sender` is a member of it. Somebody else's move
    // (or the user's on another device) waits for the user's tap.
    let record = super::calls::CallMoveRecord {
      by: sender,
      added: added.clone(),
      maybe_call: *maybe_call,
      is_requested_here: false,
    };
    return super::calls::note_call_move(store, conversation, *meeting, &record);
  }
  let target = match content {
    MessageContent::Edit { target, .. }
    | MessageContent::Delete { target }
    | MessageContent::Pin { target, .. } => *target,
    _ => return Ok(()),
  };
  // A placeholder (issue #888) is no target yet: the control waits like one for a target not here,
  // and applies when the target is decrypted (`apply_early_controls`). Applied to the placeholder,
  // decrypting would overwrite it, bringing deleted text back and losing the edit or pin.
  let Some(mut stored) = store
    .message(conversation, target)?
    .filter(|stored| stored.maybe_unreadable.is_none())
  else {
    // History loads newest first, so edits, deletions and pins arrive before their target there.
    return keep_early_control(
      store,
      conversation,
      target,
      sender,
      content,
      at_ms,
      maybe_server_id,
    );
  };
  // A deleted message stays deleted; a line this device derived (from a commit, issue #811, or
  // from the server's record, issue #818) is no message anybody sent, so nobody's edit, delete or
  // pin changes it.
  if matches!(stored.content, MessageContent::Deleted) || stored.is_local_line() {
    return Ok(());
  }
  let is_own = stored.sender_user_id == sender;
  match content {
    MessageContent::Edit { body, .. } if is_own => {
      if let Some(edit_id) = maybe_server_id {
        if stored.maybe_edit_id.is_some_and(|applied| applied >= edit_id) {
          // A later edit is already applied: this one lost.
          return Ok(());
        }
        stored.maybe_edit_id = Some(edit_id);
      }
      let MessageContent::Text { body: old, .. } = &mut stored.content else {
        return Ok(());
      };
      if old != body {
        let previous = std::mem::replace(old, body.clone());
        stored.edits.push(EditRecord {
          body: previous,
          replaced_at_ms: at_ms,
        });
        stored.is_edited = true;
      } else if maybe_server_id.is_none() {
        return Ok(());
      }
    },
    MessageContent::Delete { .. } if is_own => {
      stored.content = MessageContent::Deleted;
      stored.edits.clear();
    },
    MessageContent::Pin { is_pinned, .. } => stored.is_pinned = *is_pinned,
    _ => return Ok(()),
  }
  store.upsert_message(&stored)?;
  Ok(())
}

/// An edit, deletion or pin that arrived before the message it changes, kept in
/// [`Table::SyncState`] until that message arrives.
#[derive(Serialize, Deserialize)]
struct EarlyControl {
  sender: UserId,
  content: MessageContent,
  at_ms: i64,
  /// Missing in records of older versions.
  #[serde(rename = "server_id", default, skip_serializing_if = "Option::is_none")]
  maybe_server_id: Option<MessageId>,
}

fn keep_early_control(
  store: &impl LocalStore,
  conversation: ConversationId,
  target: ClientMessageId,
  sender: UserId,
  content: &MessageContent,
  at_ms: i64,
  maybe_server_id: Option<MessageId>,
) -> Result<(), CoreError> {
  let key = early_controls_key(conversation, target);
  let mut early: Vec<EarlyControl> = records::get_json(store, Table::SyncState, &key)?.unwrap_or_default();
  early.push(EarlyControl {
    sender,
    content: content.clone(),
    at_ms,
    maybe_server_id,
  });
  records::put_json(store, Table::SyncState, &key, &early)
}

/// Applies the controls that arrived before `target`, in the order they were sent.
fn apply_early_controls(
  store: &impl LocalStore,
  conversation: ConversationId,
  target: ClientMessageId,
) -> Result<(), CoreError> {
  let key = early_controls_key(conversation, target);
  let Some(mut early) = records::get_json::<Vec<EarlyControl>>(store, Table::SyncState, &key)? else {
    return Ok(());
  };
  store.delete(Table::SyncState, &key)?;
  early.sort_by_key(|control| (control.at_ms, control.maybe_server_id));
  for control in early {
    apply_control_with(
      store,
      conversation,
      control.sender,
      &control.content,
      control.at_ms,
      control.maybe_server_id,
    )?;
  }
  Ok(())
}

fn early_controls_key(conversation: ConversationId, target: ClientMessageId) -> String {
  format!("early-controls/{conversation}/{target}")
}

/// Where a conversation's bookmarks bar is kept: the newest `Bookmarks` message that was applied.
/// Marks in [`Table::SyncState`] up to which imported message a conversation's imported history
/// was looked at.
fn imported_key(conversation: ConversationId) -> String {
  format!("imported_loaded/{conversation}")
}

/// Marks in [`Table::SyncState`] that a conversation's first page of history was loaded.
fn history_key(conversation: ConversationId) -> String {
  format!("history_loaded/{conversation}")
}

pub(crate) fn bookmarks_key(conversation: ConversationId) -> String {
  format!("bookmarks/{conversation}")
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;
  use crate::store::MemoryStore;

  #[test]
  fn edits_converge_on_the_highest_server_id_in_any_order() {
    let conversation: ConversationId = id(10);
    let target = ClientMessageId::from_bytes([1; 16]);
    let author: UserId = id(3);
    // Arriving in order, out of order (history loads newest first), and with an own pending edit.
    for order in [[(200, "v1"), (300, "v2")], [(300, "v2"), (200, "v1")]] {
      let store = MemoryStore::new();
      store
        .upsert_message(&original(conversation, target, author))
        .unwrap();
      apply_control(&store, conversation, author, &edit(target, "pending"), 50).unwrap();
      for (server_id, body) in order {
        apply_control_with(
          &store,
          conversation,
          author,
          &edit(target, body),
          server_id,
          Some(id(server_id)),
        )
        .unwrap();
      }
      let stored = store.message(conversation, target).unwrap().unwrap();
      assert!(
        matches!(&stored.content, MessageContent::Text { body, .. } if body == "v2"),
        "{order:?}: {:?}",
        stored.content
      );
      assert_eq!(stored.maybe_edit_id, Some(id(300)));
    }
  }

  /// Review of #888 (H1): a delete, edit or pin of a message still a placeholder waits like one for
  /// a target not here, and applies once the target is decrypted; applied to the placeholder,
  /// decrypting would have overwritten it.
  mod controls_before_their_target_decrypts {
    use super::*;

    #[test]
    fn a_delete_stays_deleted() {
      let (store, conversation, target, author) = placeholder_target();
      apply_control_with(
        &store,
        conversation,
        author,
        &MessageContent::Delete { target },
        200,
        Some(id(200)),
      )
      .unwrap();
      assert!(
        store
          .message(conversation, target)
          .unwrap()
          .unwrap()
          .maybe_unreadable
          .is_some()
      );
      let stored = decrypt(&store, conversation, target, author);
      assert!(
        matches!(stored.content, MessageContent::Deleted),
        "{:?}",
        stored.content
      );
    }

    #[test]
    fn an_edit_is_kept() {
      let (store, conversation, target, author) = placeholder_target();
      apply_control_with(
        &store,
        conversation,
        author,
        &edit(target, "v1"),
        200,
        Some(id(200)),
      )
      .unwrap();
      let stored = decrypt(&store, conversation, target, author);
      assert!(matches!(&stored.content, MessageContent::Text { body, .. } if body == "v1"));
      assert_eq!(stored.maybe_edit_id, Some(id(200)));
    }

    #[test]
    fn a_pin_is_kept() {
      let (store, conversation, target, author) = placeholder_target();
      let pin = MessageContent::Pin {
        target,
        is_pinned: true,
      };
      apply_control_with(&store, conversation, author, &pin, 200, Some(id(200))).unwrap();
      assert!(decrypt(&store, conversation, target, author).is_pinned);
    }

    fn placeholder_target() -> (MemoryStore, ConversationId, ClientMessageId, UserId) {
      let (conversation, target, author) = (id(10), ClientMessageId::from_bytes([1; 16]), id(3));
      let store = MemoryStore::new();
      let mut row = original(conversation, target, author);
      row.content = MessageContent::Unknown;
      row.maybe_unreadable = Some(Unreadable::Waiting { since_ms: 0 });
      store.upsert_message(&row).unwrap();
      (store, conversation, target, author)
    }

    /// What `receive_message` does once the target decrypts.
    fn decrypt(
      store: &MemoryStore,
      conversation: ConversationId,
      target: ClientMessageId,
      author: UserId,
    ) -> StoredMessage {
      store
        .upsert_message(&original(conversation, target, author))
        .unwrap();
      apply_early_controls(store, conversation, target).unwrap();
      store.message(conversation, target).unwrap().unwrap()
    }
  }

  /// A device's own client over the fake server, receiving envelopes built from a real message of
  /// its own: with another client message ID it fails its signature, with the key ID changed its
  /// key is unknown, with another version byte it is of a newer version.
  #[cfg(not(target_arch = "wasm32"))]
  mod placeholders {
    use std::sync::Arc;

    use super::*;
    use crate::ClientSettings;
    use crate::api::fake::{FakeServer, FakeServerApi};
    use crate::clock::ManualClock;
    use crate::store::MessageFilter;

    type TestClient = Client<FakeServerApi, Arc<MemoryStore>>;

    #[tokio::test]
    async fn a_message_that_fails_its_signature_is_a_failed_placeholder() {
      let (mut client, conversation, valid, _) = client_with_message(ClientSettings::default()).await;
      let tampered = envelope(&valid, 2, 1_000, |_| {});
      client.receive_message(tampered.clone()).unwrap();
      assert_eq!(state(&client, &tampered), Some(Unreadable::Failed));
      assert_eq!(client.undecrypted_count().unwrap(), 0, "not kept for a retry");
      assert_eq!(
        client
          .store
          .edge_server_id(conversation, MessageEdge::Newest)
          .unwrap(),
        Some(valid.id),
        "no cursor moves on a placeholder"
      );
    }

    #[tokio::test]
    async fn garbage_under_a_waiting_message_s_client_id_changes_nothing() {
      let (mut client, _, valid, _) = client_with_message(ClientSettings::default()).await;
      let waiting = envelope(&valid, 2, 1_000, unknown_key);
      client.receive_message(waiting.clone()).unwrap();
      assert!(matches!(
        state(&client, &waiting),
        Some(Unreadable::Waiting { .. })
      ));

      // The server re-delivers garbage (a failing signature) under the same client message ID.
      let forged = MessageResponse {
        id: id(valid.id.snowflake().get() + 2_000),
        ciphertext: envelope(&valid, 3, 0, |_| {}).ciphertext,
        ..waiting.clone()
      };
      client.receive_message(forged).unwrap();
      assert!(matches!(
        state(&client, &waiting),
        Some(Unreadable::Waiting { .. })
      ));
      let parked: Option<MessageResponse> =
        records::get_json(&client.store, Table::Undecrypted, &undecrypted_key(&waiting)).unwrap();
      assert_eq!(
        parked.map(|parked| parked.id),
        Some(waiting.id),
        "the real ciphertext is kept"
      );
    }

    #[tokio::test]
    async fn a_message_of_a_newer_version_is_kept_and_not_called_failed() {
      let (mut client, _, valid, _) = client_with_message(ClientSettings::default()).await;
      let newer = envelope(&valid, 2, 1_000, |bytes| bytes[0] = 0x7f);
      client.receive_message(newer.clone()).unwrap();
      assert_eq!(state(&client, &newer), Some(Unreadable::NotAvailable));
      assert_eq!(
        client.undecrypted_count().unwrap(),
        1,
        "kept: an update may read it"
      );
    }

    #[tokio::test]
    async fn a_device_in_the_group_waits_until_the_long_wait_ran_out() {
      let (mut client, conversation, valid, clock) = client_with_message(ClientSettings::default()).await;
      assert!(client.has_group(conversation).unwrap());
      let waiting = envelope(&valid, 2, 1_000, unknown_key);
      client.receive_message(waiting.clone()).unwrap();

      // An hour: the user's phone may just be off. Still waiting.
      clock.advance(60 * 60 * 1000);
      assert!(!client.settle_missing_keys().unwrap());
      assert!(matches!(
        state(&client, &waiting),
        Some(Unreadable::Waiting { .. })
      ));

      clock.advance(3 * 24 * 60 * 60 * 1000);
      assert!(client.settle_missing_keys().unwrap());
      assert_eq!(state(&client, &waiting), Some(Unreadable::NotAvailable));
      assert_eq!(crate::client::key_catch_up(&client.store).unwrap(), None);
    }

    #[tokio::test]
    async fn failed_placeholders_and_parked_messages_are_bounded() {
      let settings = ClientSettings {
        max_failed_placeholders: 2,
        max_parked_per_conversation: 1,
        ..ClientSettings::default()
      };
      let (mut client, conversation, valid, clock) = client_with_message(settings).await;
      for client_id in 2..6 {
        client
          .receive_message(envelope(&valid, client_id, i64::from(client_id) * 1_000, |_| {}))
          .unwrap();
      }
      let failed = client
        .store
        .messages(conversation, MessageFilter::All)
        .unwrap()
        .into_iter()
        .filter(|message| message.maybe_unreadable == Some(Unreadable::Failed))
        .count();
      assert_eq!(failed, 2);

      for client_id in 10..13 {
        client
          .receive_message(envelope(
            &valid,
            client_id,
            i64::from(client_id) * 1_000,
            unknown_key,
          ))
          .unwrap();
      }
      clock.advance(4 * 24 * 60 * 60 * 1000);
      client.settle_missing_keys().unwrap();
      client.retry_undecrypted(conversation).unwrap();
      assert_eq!(
        client.undecrypted_count().unwrap(),
        1,
        "the oldest given up on go first"
      );
    }

    #[tokio::test]
    async fn a_month_with_a_placeholder_is_not_published_as_an_index_segment() {
      let (mut readable, _, _, clock) = client_with_message(ClientSettings::default()).await;
      clock.advance(40 * 24 * 60 * 60 * 1000);
      assert_eq!(
        readable.publish_index_segments().await.unwrap(),
        1,
        "a complete month is published"
      );

      let (mut holey, _, valid, clock) = client_with_message(ClientSettings::default()).await;
      holey
        .receive_message(envelope(&valid, 2, 1_000, unknown_key))
        .unwrap();
      clock.advance(40 * 24 * 60 * 60 * 1000);
      assert_eq!(holey.publish_index_segments().await.unwrap(), 0);
    }

    async fn client_with_message(
      settings: ClientSettings,
    ) -> (TestClient, ConversationId, MessageResponse, Arc<ManualClock>) {
      let server = FakeServer::new();
      let api = server.add_user("Alice");
      let clock = Arc::new(ManualClock::new(1_800_000_000_000));
      let options = crate::EnrollmentOptions {
        device_name: "test".into(),
        secret: crate::EnrollmentSecret::Passphrase(teamcraft_crypto::secret::SecretBytes::new(
          b"passphrase".to_vec(),
        )),
        argon2: teamcraft_crypto::backup::Argon2Settings {
          memory_kib: 64,
          iterations: 1,
          parallelism: 1,
        },
        maybe_attestation: None,
      };
      let mut client = crate::PendingEnrollment::new()
        .complete(
          api.clone(),
          Arc::new(MemoryStore::new()),
          Box::new(clock.clone()),
          settings,
          options,
        )
        .await
        .unwrap();
      let space = api.list_spaces().await.unwrap().spaces[0].id;
      let conversation = client.create_channel(space, "general").await.unwrap();
      client.send_text(conversation, None, "hello").await.unwrap();
      let valid = api
        .list_messages(conversation, &teamcraft_proto::ListMessagesQuery::default())
        .await
        .unwrap()
        .messages
        .into_iter()
        .max_by_key(|message| message.id)
        .unwrap();
      (client, conversation, valid, clock)
    }

    /// `valid` under another client message ID and a later server ID, its ciphertext changed by
    /// `change`.
    fn envelope(
      valid: &MessageResponse,
      client_id: u8,
      later_by: i64,
      change: impl FnOnce(&mut [u8]),
    ) -> MessageResponse {
      let mut bytes = valid.ciphertext.as_slice().to_vec();
      change(bytes.as_mut_slice());
      MessageResponse {
        id: id(valid.id.snowflake().get() + later_by),
        client_message_id: ClientMessageId::from_bytes([client_id; 16]),
        ciphertext: Base64Bytes::new(bytes),
        ..valid.clone()
      }
    }

    /// The content key ID follows the version byte.
    fn unknown_key(bytes: &mut [u8]) {
      bytes[1] ^= 0xff;
    }

    fn state(client: &TestClient, message: &MessageResponse) -> Option<Unreadable> {
      client
        .store
        .message(message.conversation_id, message.client_message_id)
        .unwrap()
        .and_then(|stored| stored.maybe_unreadable)
    }
  }

  fn edit(target: ClientMessageId, body: &str) -> MessageContent {
    MessageContent::Edit {
      target,
      body: body.into(),
    }
  }

  fn original(conversation: ConversationId, target: ClientMessageId, author: UserId) -> StoredMessage {
    StoredMessage {
      conversation_id: conversation,
      client_message_id: target,
      maybe_server_id: Some(id(100)),
      sender_user_id: author,
      maybe_thread_id: None,
      sent_at_ms: 1,
      content: MessageContent::text("v0", Vec::new()),
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    }
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
