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

/// The list as this device read it last, in its encrypted store, for readers without the keys.
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
  /// The server refused sealing this version again in this epoch (the sender may not post there any
  /// more): not tried again until the entry changes or the group moves on.
  #[serde(rename = "seal_refused", default, skip_serializing_if = "Option::is_none")]
  pub maybe_seal_refused: Option<SealRefused>,
  /// The server removed it after its retention (docs/scheduled-messages.md): kept here, so the user
  /// can still copy the text, until dismissed.
  #[serde(rename = "removed", default, skip_serializing_if = "std::ops::Not::not")]
  pub is_removed_by_server: bool,
}

/// What a refusal to seal was about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SealRefused {
  pub version: u64,
  pub epoch: u64,
}

/// What sealing again ran into, and so what to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SealFailure {
  /// The group moved between catching up and asking: catch up and try again.
  EpochMoved,
  /// Another device was first: it drops its work and reads the list.
  LostRace,
  /// The server says no (403, 404), or the group is gone from this device: not tried again until
  /// something changes.
  Refused,
  /// Offline, the server in trouble, or the sign-in ran out: stops this round.
  Later,
  /// Anything else (the group can't be caught up here, say): tried again later, paced by the caller.
  Other,
}

impl SealFailure {
  fn of(error: &CoreError) -> Self {
    if error.is_retry_later() {
      return Self::Later;
    }
    let api = match error {
      CoreError::Api(api) => api,
      CoreError::NoGroup(_) => return Self::Refused,
      _ => return Self::Other,
    };
    match api.error_code() {
      Some(ErrorCode::EpochConflict) => Self::EpochMoved,
      Some(ErrorCode::VersionConflict) => Self::LostRace,
      _ => match api {
        ApiError::Status {
          status: 403 | 404, ..
        } => Self::Refused,
        _ => Self::Other,
      },
    }
  }
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

  /// Whether the gateway said the list changed since it was read last. Taken: a failed read asks again
  /// through [`Self::note_scheduled_list_stale`].
  pub fn take_scheduled_list_stale(&mut self) -> bool {
    std::mem::take(&mut self.is_scheduled_list_stale)
  }

  pub fn note_scheduled_list_stale(&mut self) {
    self.is_scheduled_list_stale = true;
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
        let maybe_known = known
          .entries
          .iter()
          .find(|entry| entry.response.id == response.id);
        // What didn't change isn't opened again: a list read costs what changed.
        let maybe_same = maybe_known
          .filter(|entry| entry.response.version == response.version && entry.maybe_content.is_some());
        let maybe_content = match maybe_same {
          Some(same) => same.maybe_content.clone(),
          None => self.read_scheduled(&response),
        };
        let maybe_seal_refused = maybe_known
          .and_then(|entry| entry.maybe_seal_refused)
          .filter(|refused| refused.version == response.version);
        ScheduledEntry {
          response,
          maybe_content,
          maybe_seal_refused,
          is_removed_by_server: false,
        }
      })
      .collect::<Vec<_>>();
    let removed = removed_by_server(known.entries, &entries, listed.server_now);
    let entries = entries.into_iter().chain(removed).collect();
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
        Ok(answer) => {
          self
            .settle_retry(conversation, &request, &answer, &content)
            .await?;
          break;
        },
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

  /// The server answers a known ID with what it holds (a retry after a lost answer), and a sealing is
  /// never the same twice, so its own ciphertext coming back means it took this one. Otherwise the user
  /// may have picked another time, or another reply target, since the first try: what the user asked
  /// for now is what counts. A thread can't change on a held message, so that one starts over.
  async fn settle_retry(
    &mut self,
    conversation: ConversationId,
    request: &ScheduleMessageRequest,
    answer: &ScheduledMessageResponse,
    content: &MessageContent,
  ) -> Result<(), CoreError> {
    if answer.ciphertext == request.sealed.ciphertext {
      return Ok(());
    }
    if answer.maybe_thread_id != request.maybe_thread_id {
      self.api.cancel_scheduled_message(answer.id).await?;
      self.api.schedule_message(conversation, request).await?;
      return Ok(());
    }
    let is_same_content = self
      .read_scheduled(answer)
      .is_some_and(|held| without_heads(held) == without_heads(content.clone()));
    if answer.due_at == request.due_at && is_same_content {
      return Ok(());
    }
    self
      .api
      .update_scheduled_message(
        answer.id,
        &UpdateScheduledMessageRequest {
          expected_version: answer.version,
          due_at: request.due_at,
          maybe_sealed: Some(request.sealed.clone()),
        },
      )
      .await?;
    Ok(())
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

  /// Cancels a held message; one the server removed already only goes from this device.
  pub(crate) async fn cancel_scheduled(&mut self, id: ScheduledMessageId) -> Result<(), CoreError> {
    let mut list = read_list(&self.store)?;
    if list
      .entries
      .iter()
      .any(|entry| entry.response.id == id && entry.is_removed_by_server)
    {
      list.entries.retain(|entry| entry.response.id != id);
      return write_list(&self.store, &list);
    }
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
    let mut has_lost_race = false;
    let mut maybe_failure = None;
    for entry in &list.entries {
      if !self.needs_sealing_again(entry)? {
        continue;
      }
      let id = entry.response.id;
      let mut outcome = self.seal_again(id).await;
      // Sealing caught the group up; it moved once more before the answer. Once more, then later.
      if matches!(&outcome, Err(error) if SealFailure::of(error) == SealFailure::EpochMoved) {
        outcome = self.seal_again(id).await;
      }
      match outcome {
        Ok(()) => sealed += 1,
        Err(error) => match SealFailure::of(&error) {
          SealFailure::Later => {
            if sealed > 0 {
              self.refresh_scheduled().await?;
            }
            return Err(error);
          },
          SealFailure::LostRace => has_lost_race = true,
          SealFailure::Refused => {
            tracing::debug!(%error, scheduled_message_id = %id, "sealing a scheduled message again was refused");
            let epoch = self
              .group_or_error(entry.response.conversation_id)
              .map_or(0, |group| group.epoch());
            self.note_seal_refused(id, entry.response.version, epoch)?;
          },
          SealFailure::EpochMoved | SealFailure::Other => {
            tracing::debug!(%error, scheduled_message_id = %id, "a scheduled message wasn't sealed again yet");
            maybe_failure = Some(error);
          },
        },
      }
    }
    if sealed > 0 || has_lost_race {
      self.refresh_scheduled().await?;
    }
    match maybe_failure {
      // The caller paces the next round; this isn't to be tried on every tick.
      Some(error) => Err(error),
      None => Ok(sealed),
    }
  }

  /// Whether this device should and can seal the entry again.
  fn needs_sealing_again(&mut self, entry: &ScheduledEntry) -> Result<bool, CoreError> {
    let response = &entry.response;
    if entry.maybe_content.is_none() {
      return Ok(false);
    }
    // Only these can be solved by sealing: the others stay until the user decides.
    match &response.maybe_hold {
      None | Some(ScheduledHoldReason::StaleEpoch) => {},
      Some(_) => return Ok(false),
    }
    // Without the group this device can't seal: it comes back with a welcome, or the sender was removed
    // and the server's check says so at the time.
    if !self.has_group(response.conversation_id)? {
      return Ok(false);
    }
    let epoch = self.group_or_error(response.conversation_id)?.epoch();
    // A refusal counts for its version in its epoch: once the group moved on, it may not hold.
    if let Some(refused) = entry.maybe_seal_refused
      && refused.version == response.version
      && refused.epoch == epoch
    {
      return Ok(false);
    }
    if response.maybe_hold.is_some() || !response.is_epoch_current {
      return Ok(true);
    }
    // The server's word when the list was read is old by now; this device may have merged a commit.
    Ok(epoch > response.mls_epoch)
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
    let (mentioned_user_ids, is_urgent, is_channel_mention) = match content {
      MessageContent::Text { body, mentions, meta } => (
        mentions.clone(),
        meta.priority == Priority::Urgent,
        teamcraft_proto::addresses_everyone(body),
      ),
      _ => (Vec::new(), false, false),
    };
    Ok(SealedScheduledMessage {
      device_id: self.account.device_id,
      content_key_id: Base64Bytes::new(encrypted.content_key_id().as_bytes().to_vec()),
      ciphertext: Base64Bytes::new(encrypted.to_bytes()?),
      mentioned_user_ids,
      is_urgent,
      is_channel_mention,
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

  fn note_seal_refused(&self, id: ScheduledMessageId, version: u64, epoch: u64) -> Result<(), CoreError> {
    let mut list = read_list(&self.store)?;
    if let Some(entry) = list.entries.iter_mut().find(|entry| entry.response.id == id) {
      entry.maybe_seal_refused = Some(SealRefused { version, epoch });
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
    self
      .ui_backend()
      .text_mentioning(conversation, body, meta.clone())
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

/// The entries the server no longer lists because it removed them at their removal date, as far as this
/// device read them. Something cancelled or released elsewhere has no removal date that passed.
fn removed_by_server(
  known: Vec<ScheduledEntry>,
  listed: &[ScheduledEntry],
  server_now: UnixMillis,
) -> Vec<ScheduledEntry> {
  known
    .into_iter()
    .filter(|entry| {
      !listed
        .iter()
        .any(|listed| listed.response.id == entry.response.id)
    })
    .filter(|entry| {
      entry.is_removed_by_server
        || entry
          .response
          .maybe_removes_at
          .is_some_and(|removes_at| removes_at.0 <= server_now.0)
    })
    .filter(|entry| entry.maybe_content.is_some())
    .map(|entry| ScheduledEntry {
      is_removed_by_server: true,
      ..entry
    })
    .collect()
}

/// A message's content without what sealing adds to it at that moment.
fn without_heads(mut content: MessageContent) -> MessageContent {
  if let MessageContent::Text { meta, .. } = &mut content {
    meta.maybe_workspace_head = None;
    meta.maybe_channel_roles_head = None;
  }
  content
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

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_a_refusal_stops_sealing_for_good() {
    for (error, expected) in [
      (status(403, ErrorCode::Forbidden), SealFailure::Refused),
      (
        status(404, ErrorCode::ScheduledMessageNotFound),
        SealFailure::Refused,
      ),
      (status(409, ErrorCode::EpochConflict), SealFailure::EpochMoved),
      (status(409, ErrorCode::VersionConflict), SealFailure::LostRace),
      (status(401, ErrorCode::Unauthorized), SealFailure::Later),
      (status(500, ErrorCode::Internal), SealFailure::Later),
      (status(429, ErrorCode::RateLimited), SealFailure::Later),
      (
        CoreError::Api(ApiError::Network("down".into())),
        SealFailure::Later,
      ),
      (status(400, ErrorCode::InvalidRequest), SealFailure::Other),
      (CoreError::NoGroup(conversation()), SealFailure::Refused),
      (CoreError::NoContentKey(conversation()), SealFailure::Other),
    ] {
      assert_eq!(SealFailure::of(&error), expected, "{error}");
    }
  }

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(teamcraft_proto::Snowflake::new(1).unwrap())
  }

  fn status(status: u16, code: ErrorCode) -> CoreError {
    CoreError::Api(ApiError::status(status, code, "x"))
  }
}
