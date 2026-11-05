//! "Send later" (docs/scheduled-messages.md, e2ee.md §4.4): a device seals a message now, in the group's
//! current epoch, and the server releases it at its time while the group is still in that epoch. Once
//! the group moved on, any online device of the sender seals it again under the current key, ahead of
//! the time when it can. The server's list holds only ciphertext; each device reads it like any message
//! of the conversation and keeps what it read in its own encrypted store for the screens.

use serde::{Deserialize, Serialize};
use teamcraft_client_api::actions::{MessagePriority, OutgoingMessage};
use teamcraft_crypto::message::EncryptedMessage;
use teamcraft_proto::{
  Base64Bytes, ClientMessageId, ConversationId, ErrorCode, Id, MessageId, MessageResponse,
  ScheduleMessageRequest, ScheduledHoldReason, ScheduledMessageId, ScheduledMessageResponse,
  SealedScheduledMessage, UnixMillis, UpdateScheduledMessageRequest,
};

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::clock::Clock;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageContent, Priority, Table};
use crate::ui_backend::CoreChatBackend;

/// The list as this device read it last, next to the account data for readers without the keys.
pub(crate) const SCHEDULED_LIST_KEY: &str = "scheduled_messages";

/// What this device knows of the user's held messages.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct ScheduledList {
  #[serde(default)]
  pub entries: Vec<ScheduledEntry>,
  /// The server's clock against this device's when the list was read, in milliseconds. Only for
  /// computing "in 1 hour": the server's clock decides when a message goes out.
  #[serde(default)]
  pub server_offset_ms: i64,
}

/// One held message and what this device could read of it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ScheduledEntry {
  pub response: ScheduledMessageResponse,
  /// `None`: this device doesn't hold the key yet.
  #[serde(rename = "content", default, skip_serializing_if = "Option::is_none")]
  pub maybe_content: Option<MessageContent>,
  /// The server refused sealing this version again (the sender can't post there any more): not
  /// tried again until the entry changes.
  #[serde(
    rename = "seal_refused_version",
    default,
    skip_serializing_if = "Option::is_none"
  )]
  pub maybe_seal_refused_version: Option<u64>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// [`Self::schedule_message`] and the other changes for a host that calls the client itself and
  /// wants the answer (the mobile bridge).
  pub async fn schedule_now_for(
    &mut self,
    message: OutgoingMessage,
    due_at_ms: i64,
  ) -> Result<(), CoreError> {
    self.schedule_message(message, due_at_ms).await
  }

  pub async fn change_scheduled(
    &mut self,
    id: ScheduledMessageId,
    body: String,
    due_at_ms: i64,
  ) -> Result<(), CoreError> {
    self.update_scheduled(id, body, due_at_ms).await
  }

  pub async fn drop_scheduled(&mut self, id: ScheduledMessageId) -> Result<(), CoreError> {
    self.cancel_scheduled(id).await
  }

  pub async fn release_scheduled(&mut self, id: ScheduledMessageId) -> Result<(), CoreError> {
    self.send_scheduled_now(id).await
  }

  /// A push said the group of a held message changed (docs/scheduled-messages.md): reads the list and
  /// seals again what it names. The loser of two devices racing gets a conflict and drops its work.
  pub async fn seal_after_wake(&mut self) -> Result<(), CoreError> {
    self.refresh_scheduled().await?;
    self.seal_stale_scheduled().await.map(|_| ())
  }

  /// Reads the list from the server and what it holds with this device's keys. A server from before
  /// this feature has no list: that is an empty one.
  pub async fn refresh_scheduled(&mut self) -> Result<(), CoreError> {
    let sent_at_ms = self.now_ms();
    let listed = match self.api.list_scheduled_messages().await {
      Ok(listed) => listed,
      Err(ApiError::Status { status: 404, .. }) => return Ok(()),
      Err(error) => return Err(error.into()),
    };
    let known = read_list(&self.store)?;
    let local_middle = sent_at_ms.saturating_add(self.now_ms()) / 2;
    let entries = listed
      .messages
      .into_iter()
      .map(|response| {
        let maybe_refused = known
          .entries
          .iter()
          .find(|entry| entry.response.id == response.id && entry.response.version == response.version)
          .and_then(|entry| entry.maybe_seal_refused_version);
        let maybe_content = self.read_scheduled(&response);
        ScheduledEntry {
          response,
          maybe_content,
          maybe_seal_refused_version: maybe_refused,
        }
      })
      .collect();
    write_list(
      &self.store,
      &ScheduledList {
        entries,
        server_offset_ms: listed.server_now.0.saturating_sub(local_middle),
      },
    )
  }

  /// Seals a message under the current key and hands it to the server for `due_at_ms` (server time).
  /// Needs the server: offline this fails, and the composer keeps the text.
  pub(crate) async fn schedule_message(
    &mut self,
    message: OutgoingMessage,
    due_at_ms: i64,
  ) -> Result<(), CoreError> {
    let conversation = message.conversation;
    let maybe_thread = message.maybe_thread;
    let body = message.body.clone();
    let id = message
      .maybe_client_message_id
      .unwrap_or_else(records::new_client_message_id);
    let content = self.scheduled_content(message);
    // A commit may land between catching up and asking; once more, on the newer epoch.
    let mut is_retry = false;
    loop {
      let sealed = self.seal_scheduled(conversation, id, &content).await?;
      let request = ScheduleMessageRequest {
        client_message_id: id,
        maybe_thread_id: maybe_thread,
        sealed,
        due_at: UnixMillis(due_at_ms),
      };
      match self.api.schedule_message(conversation, &request).await {
        Ok(_) => break,
        Err(error) if !is_retry && error.error_code() == Some(&ErrorCode::EpochConflict) => is_retry = true,
        Err(error) => return Err(error.into()),
      }
    }
    // It left the composer: the draft of the same text goes too, on the user's other devices as well.
    self.update_account_data(|data, now| {
      data.draft(conversation, maybe_thread) == Some(body.as_str())
        && data.set_draft(conversation, maybe_thread, "", now)
    })?;
    self.refresh_scheduled().await
  }

  /// Changes the text and time of a held message. A changed text is sealed again; a new time alone
  /// isn't, so the server can hold it on.
  pub(crate) async fn update_scheduled(
    &mut self,
    id: ScheduledMessageId,
    body: String,
    due_at_ms: i64,
  ) -> Result<(), CoreError> {
    let Some(entry) = self.scheduled_entry(id)? else {
      return Ok(());
    };
    let maybe_content = entry.maybe_content.as_ref().and_then(|content| match content {
      MessageContent::Text { body: old, meta, .. } if *old != body => {
        Some(self.edited_content(entry.response.conversation_id, &body, meta))
      },
      _ => None,
    });
    let maybe_sealed = match maybe_content {
      Some(content) => Some(
        self
          .seal_scheduled(
            entry.response.conversation_id,
            entry.response.client_message_id,
            &content,
          )
          .await?,
      ),
      None => None,
    };
    let outcome = self
      .api
      .update_scheduled_message(
        id,
        &UpdateScheduledMessageRequest {
          expected_version: entry.response.version,
          due_at: UnixMillis(due_at_ms),
          maybe_sealed,
        },
      )
      .await;
    self.refresh_scheduled().await?;
    outcome.map(|_| ()).map_err(CoreError::from)
  }

  pub(crate) async fn cancel_scheduled(&mut self, id: ScheduledMessageId) -> Result<(), CoreError> {
    let outcome = self.api.cancel_scheduled_message(id).await;
    self.refresh_scheduled().await?;
    outcome.map_err(CoreError::from)
  }

  /// Sends a held message now. If the server finds it under an old epoch it is sealed again first.
  pub(crate) async fn send_scheduled_now(&mut self, id: ScheduledMessageId) -> Result<(), CoreError> {
    let first = self.api.send_scheduled_message(id).await;
    let outcome = match first {
      Err(error) if error.error_code() == Some(&ErrorCode::EpochConflict) => {
        self.refresh_scheduled().await?;
        self.seal_again(id).await?;
        self.api.send_scheduled_message(id).await
      },
      other => other,
    };
    self.refresh_scheduled().await?;
    let sent = outcome?;
    self.receive_sent_scheduled(sent.message)
  }

  /// Seals again what the group left behind: ahead of the time wherever a device of the sender is
  /// online, and after it for what is waiting for one. A local read while nothing needs it. Returns
  /// how many messages were sealed again.
  pub async fn seal_stale_scheduled(&mut self) -> Result<usize, CoreError> {
    let list = read_list(&self.store)?;
    let mut sealed = 0;
    for entry in &list.entries {
      if !self.needs_sealing_again(entry)? {
        continue;
      }
      match self.seal_again(entry.response.id).await {
        Ok(()) => sealed += 1,
        Err(error) if error.is_transient() => return Err(error),
        Err(error) => {
          // Another device was first, or the sender may not post there any more: not a busy loop.
          tracing::debug!(%error, scheduled_message_id = %entry.response.id, "a scheduled message wasn't sealed again");
          self.note_seal_refused(entry.response.id, entry.response.version)?;
        },
      }
    }
    if sealed > 0 {
      self.refresh_scheduled().await?;
    }
    Ok(sealed)
  }

  /// Whether this device should and can seal the entry again.
  fn needs_sealing_again(&mut self, entry: &ScheduledEntry) -> Result<bool, CoreError> {
    let response = &entry.response;
    if entry.maybe_content.is_none() || entry.maybe_seal_refused_version == Some(response.version) {
      return Ok(false);
    }
    // Only these can be solved by sealing: the others stay until the user decides.
    match &response.maybe_hold {
      None | Some(ScheduledHoldReason::StaleEpoch) => {},
      Some(_) => return Ok(false),
    }
    if response.maybe_hold.is_some() || !response.is_epoch_current {
      return Ok(true);
    }
    // The server's word when the list was read is old by now; this device may have merged a commit.
    Ok(
      self.has_group(response.conversation_id)?
        && self.group_or_error(response.conversation_id)?.epoch() > response.mls_epoch,
    )
  }

  /// Seals a held message again in the current epoch, keeping its time.
  async fn seal_again(&mut self, id: ScheduledMessageId) -> Result<(), CoreError> {
    let Some(entry) = self.scheduled_entry(id)? else {
      return Ok(());
    };
    let Some(content) = entry.maybe_content else {
      return Err(CoreError::NoContentKey(entry.response.conversation_id));
    };
    let sealed = self
      .seal_scheduled(
        entry.response.conversation_id,
        entry.response.client_message_id,
        &content,
      )
      .await?;
    self
      .api
      .update_scheduled_message(
        id,
        &UpdateScheduledMessageRequest {
          expected_version: entry.response.version,
          due_at: entry.response.due_at,
          maybe_sealed: Some(sealed),
        },
      )
      .await?;
    Ok(())
  }

  /// Encrypts and signs in the group's current epoch, after catching up the group: this never seals
  /// under a key a removed member still holds, as for any send (e2ee.md §4.6).
  async fn seal_scheduled(
    &mut self,
    conversation: ConversationId,
    id: ClientMessageId,
    content: &MessageContent,
  ) -> Result<SealedScheduledMessage, CoreError> {
    self.sync_mls(conversation).await?;
    let mls_epoch = self.group_or_error(conversation)?.epoch();
    let encrypted: EncryptedMessage = self.seal_message(conversation, id, content)?;
    let (mentioned_user_ids, is_urgent) = match content {
      MessageContent::Text { mentions, meta, .. } => (mentions.clone(), meta.priority == Priority::Urgent),
      _ => (Vec::new(), false),
    };
    Ok(SealedScheduledMessage {
      device_id: self.account.device_id,
      content_key_id: Base64Bytes::new(encrypted.content_key_id().as_bytes().to_vec()),
      ciphertext: Base64Bytes::new(encrypted.to_bytes()?),
      mentioned_user_ids,
      is_urgent,
      mls_epoch,
    })
  }

  /// Opens a held message like any message of the conversation. `None` if this device can't.
  fn read_scheduled(&self, response: &ScheduledMessageResponse) -> Option<MessageContent> {
    let as_message = MessageResponse {
      // Not part of what was sealed (e2ee.md §4.4); a released message gets the server's real one.
      id: MessageId::from_snowflake(response.id.snowflake()),
      client_message_id: response.client_message_id,
      conversation_id: response.conversation_id,
      sender_user_id: self.account.user_id,
      sender_device_id: response.sender_device_id,
      maybe_thread_id: response.maybe_thread_id,
      content_key_id: response.content_key_id.clone(),
      ciphertext: response.ciphertext.clone(),
      mentioned_user_ids: response.mentioned_user_ids.clone(),
      is_urgent: response.is_urgent,
    };
    match self.decrypt(&as_message) {
      Ok(content) => Some(content),
      Err(error) => {
        tracing::debug!(%error, scheduled_message_id = %response.id, "a scheduled message can't be read here yet");
        None
      },
    }
  }

  /// The answer of a send now holds the message: no need to wait for the gateway's copy.
  fn receive_sent_scheduled(&mut self, message: MessageResponse) -> Result<(), CoreError> {
    self.receive_message(message)
  }

  fn scheduled_entry(&self, id: ScheduledMessageId) -> Result<Option<ScheduledEntry>, CoreError> {
    Ok(
      read_list(&self.store)?
        .entries
        .into_iter()
        .find(|entry| entry.response.id == id),
    )
  }

  fn note_seal_refused(&self, id: ScheduledMessageId, version: u64) -> Result<(), CoreError> {
    let mut list = read_list(&self.store)?;
    if let Some(entry) = list.entries.iter_mut().find(|entry| entry.response.id == id) {
      entry.maybe_seal_refused_version = Some(version);
    }
    write_list(&self.store, &list)
  }

  /// What the composer's message says, with the mentions and the quote a send works out.
  fn scheduled_content(&self, message: OutgoingMessage) -> MessageContent {
    self.ui_backend().ui_text_content(message)
  }

  fn edited_content(
    &self,
    conversation: ConversationId,
    body: &str,
    meta: &crate::store::MessageMeta,
  ) -> MessageContent {
    MessageContent::Text {
      body: body.to_owned(),
      mentions: self.ui_backend().mentions_in(conversation, body),
      meta: meta.clone(),
    }
  }

  fn ui_backend(&self) -> CoreChatBackend<&S> {
    CoreChatBackend::new(
      &self.store,
      self.account.user_id,
      Box::new(FrozenClock(self.now_ms())),
    )
  }
}

/// The list as stored; empty when none was read yet or the record can't be read (the next read
/// replaces it).
pub(crate) fn read_list(store: &impl LocalStore) -> Result<ScheduledList, CoreError> {
  Ok(records::get_json(store, Table::Account, SCHEDULED_LIST_KEY)?.unwrap_or_default())
}

fn write_list(store: &impl LocalStore, list: &ScheduledList) -> Result<(), CoreError> {
  records::put_json(store, Table::Account, SCHEDULED_LIST_KEY, list)
}

pub(crate) fn message_priority(priority: Priority) -> MessagePriority {
  match priority {
    Priority::Important => MessagePriority::Important,
    Priority::Urgent => MessagePriority::Urgent,
    Priority::Normal | Priority::Unknown => MessagePriority::Normal,
  }
}

/// A backend for working out a message's content; nothing it stamps is kept.
struct FrozenClock(i64);

impl Clock for FrozenClock {
  fn now_ms(&self) -> i64 {
    self.0
  }
}
