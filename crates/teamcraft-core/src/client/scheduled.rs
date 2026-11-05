//! "Send later" (docs/scheduled-messages.md): the user's devices hold scheduled messages in the
//! encrypted account data, and whichever is online at or after the time sends them, encrypted then
//! under the conversation's current content key. The entry's ID becomes the message's client ID, so
//! two devices racing, or a retry after a crash, store one message on the server.

use teamcraft_client_api::actions::{MessagePriority, OutgoingMessage};
use teamcraft_client_api::scheduled::{ScheduleCheck, ScheduleRefusal, ScheduledStatus, status_of};
use teamcraft_proto::{ClientMessageId, ScheduledMessagePolicy};

use super::Client;
use crate::account_data::ScheduledEntry;
use crate::api::ServerApi;
use crate::clock::Clock;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Priority, Table};
use crate::ui_backend::CoreChatBackend;

/// The server's clock against this device's, in milliseconds (ahead is positive). Stored so the
/// UI's reads and the next start know it too.
pub(crate) const SERVER_CLOCK_KEY: &str = "scheduled/server_clock_offset";
/// Below this the offset isn't worth a write: requests take that long anyway.
const CLOCK_WRITE_THRESHOLD_MS: i64 = 1_000;

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Sends the scheduled messages whose time has come. A local read while none is. Before anything
  /// goes out it reads the newest key bag: the user may have cancelled or changed one on another
  /// device, or another device sent it. Offline that read fails and the messages wait.
  pub async fn send_due_scheduled(&mut self) -> Result<usize, CoreError> {
    if self.due_scheduled()?.is_empty() {
      return Ok(0);
    }
    self.sync_account_data().await?;
    let due = self.due_scheduled()?;
    if due.is_empty() {
      return Ok(0);
    }
    self.enqueue_scheduled(&due)?;
    // Tells the user's other devices right away; they would be stopped by the server's
    // deduplication otherwise, and the next tick tries again if this fails.
    if let Err(error) = self.upload_key_bag().await {
      tracing::debug!(%error, "sent scheduled messages not recorded on the other devices yet");
    }
    Ok(due.len())
  }

  /// Adds a message to send at `due_at_ms` (server time). The screens check the same rule first and
  /// show why; this is for what changed since they did.
  pub(crate) fn schedule_message(
    &mut self,
    message: OutgoingMessage,
    due_at_ms: i64,
  ) -> Result<(), CoreError> {
    let id = message
      .maybe_client_message_id
      .unwrap_or_else(records::new_client_message_id);
    let is_new = self.account_data().scheduled_message(id).is_none();
    self.check_schedule(&message.body, due_at_ms, is_new)?;
    let entry = ScheduledEntry {
      client_message_id: id,
      conversation_id: message.conversation,
      maybe_thread_id: message.maybe_thread,
      body: message.body,
      due_at_ms,
      maybe_reply_to: message.maybe_reply_to,
      priority: stored_priority(message.priority),
      wants_acknowledgement: message.wants_acknowledgement,
      is_silent: message.is_silent,
      also_send_to_channel: message.also_send_to_channel,
    };
    self.update_account_data(|data, now| {
      data.set_scheduled(entry, now);
      true
    })
  }

  /// Changes the text and time of a message still waiting; nothing if it went out or was cancelled
  /// meanwhile.
  pub(crate) fn update_scheduled(
    &mut self,
    id: ClientMessageId,
    body: String,
    due_at_ms: i64,
  ) -> Result<(), CoreError> {
    let Some(mut entry) = self.account_data().scheduled_message(id).cloned() else {
      return Ok(());
    };
    self.check_schedule(&body, due_at_ms, false)?;
    entry.body = body;
    entry.due_at_ms = due_at_ms;
    self.update_account_data(|data, now| {
      data.set_scheduled(entry, now);
      true
    })
  }

  pub(crate) fn cancel_scheduled(&mut self, id: ClientMessageId) -> Result<(), CoreError> {
    self.update_account_data(|data, now| data.remove_scheduled(id, now))
  }

  /// Sends a waiting message now, whatever its time or status: the user decided. It goes through the
  /// outbox like any send, so offline it waits and shows as pending in its conversation.
  pub(crate) async fn send_scheduled_now(&mut self, id: ClientMessageId) -> Result<(), CoreError> {
    let Some(entry) = self.account_data().scheduled_message(id).cloned() else {
      return Ok(());
    };
    self.enqueue_scheduled(std::slice::from_ref(&entry))?;
    self.flush_outbox().await.map(|_| ())
  }

  /// Learns the server's clock from an answer it gave between `sent_at_ms` and now.
  pub(crate) fn note_server_time(&self, server_now_ms: i64, sent_at_ms: i64) -> Result<(), CoreError> {
    let local_middle = sent_at_ms.saturating_add(self.now_ms()) / 2;
    let offset_ms = server_now_ms.saturating_sub(local_middle);
    let known = server_clock_offset_ms(&self.store);
    if offset_ms.abs_diff(known) >= CLOCK_WRITE_THRESHOLD_MS.unsigned_abs() {
      records::put_json(&self.store, Table::SyncState, SERVER_CLOCK_KEY, &offset_ms)?;
    }
    Ok(())
  }

  /// The messages this device may send now, as far as it knows.
  fn due_scheduled(&self) -> Result<Vec<ScheduledEntry>, CoreError> {
    due_entries(&self.store, self.clock.now_ms())
  }

  fn check_schedule(&self, body: &str, due_at_ms: i64, is_new: bool) -> Result<(), CoreError> {
    let policy = self.workspace_policy()?.scheduled_messages;
    let refusals = ScheduleCheck {
      body,
      due_at: due_at_ms,
      server_now: server_now_ms(&self.store, self.clock.now_ms()),
      waiting: self.account_data().scheduled_messages().len(),
      is_new,
    }
    .refusals(&policy);
    match refusals.first() {
      Some(refusal) => Err(refusal_error(*refusal)),
      None => Ok(()),
    }
  }

  /// Puts the messages into the outbox, then marks them sent. In this order: a crash in between
  /// finds the entry still waiting and enqueues the same ID again, which changes nothing.
  fn enqueue_scheduled(&mut self, entries: &[ScheduledEntry]) -> Result<(), CoreError> {
    let backend = CoreChatBackend::new(
      &self.store,
      self.account.user_id,
      Box::new(FrozenClock(self.now_ms())),
    );
    for entry in entries {
      backend.ui_send_message(outgoing(entry));
    }
    // What the backend queued is what the upkeep's flush does next.
    drop(backend.drain_commands());
    drop(backend);
    let sent: Vec<ClientMessageId> = entries.iter().map(|entry| entry.client_message_id).collect();
    self.update_account_data(|data, now| {
      let mut is_changed = false;
      for id in &sent {
        is_changed |= data.remove_scheduled(*id, now);
      }
      is_changed
    })
  }
}

/// The server's clock as this device knows it: `local_now_ms` plus the offset last measured.
pub(crate) fn server_now_ms(store: &impl LocalStore, local_now_ms: i64) -> i64 {
  local_now_ms.saturating_add(server_clock_offset_ms(store))
}

fn server_clock_offset_ms(store: &impl LocalStore) -> i64 {
  match records::get_json::<i64>(store, Table::SyncState, SERVER_CLOCK_KEY) {
    Ok(maybe_offset) => maybe_offset.unwrap_or(0),
    Err(error) => {
      tracing::warn!(%error, "the server's clock offset is unreadable");
      0
    },
  }
}

/// The entries due now: their time has come, the policy allows it, it isn't too late and this device
/// knows their conversation.
pub(crate) fn due_entries(
  store: &impl LocalStore,
  local_now_ms: i64,
) -> Result<Vec<ScheduledEntry>, CoreError> {
  let data = super::account::stored_account_data(store)?;
  if data.scheduled.is_empty() {
    return Ok(Vec::new());
  }
  let now = server_now_ms(store, local_now_ms);
  let waiting = data.scheduled_messages();
  // Soonest first, so everything after the first still in the future is too.
  if waiting.first().is_none_or(|first| first.due_at_ms > now) {
    return Ok(Vec::new());
  }
  let policy = workspace_policy(store)?;
  let mut due = Vec::new();
  for entry in waiting.into_iter().take_while(|entry| entry.due_at_ms <= now) {
    let is_known = store
      .get(Table::Conversations, &entry.conversation_id.to_string())?
      .is_some();
    if status_of(entry.due_at_ms, now, &policy, is_known) == ScheduledStatus::Due {
      due.push(entry.clone());
    }
  }
  Ok(due)
}

fn workspace_policy(store: &impl LocalStore) -> Result<ScheduledMessagePolicy, CoreError> {
  let state: Option<crate::workspace::WorkspaceState> =
    records::get_json(store, Table::Workspace, super::WORKSPACE_KEY)?;
  Ok(state.unwrap_or_default().policy.scheduled_messages)
}

fn outgoing(entry: &ScheduledEntry) -> OutgoingMessage {
  OutgoingMessage {
    conversation: entry.conversation_id,
    maybe_client_message_id: Some(entry.client_message_id),
    maybe_thread: entry.maybe_thread_id,
    body: entry.body.clone(),
    maybe_reply_to: entry.maybe_reply_to,
    priority: message_priority(entry.priority),
    wants_acknowledgement: entry.wants_acknowledgement,
    is_silent: entry.is_silent,
    also_send_to_channel: entry.also_send_to_channel,
    files: Vec::new(),
  }
}

fn stored_priority(priority: MessagePriority) -> Priority {
  match priority {
    MessagePriority::Normal => Priority::Normal,
    MessagePriority::Important => Priority::Important,
    MessagePriority::Urgent => Priority::Urgent,
  }
}

pub(crate) fn message_priority(priority: Priority) -> MessagePriority {
  match priority {
    Priority::Important => MessagePriority::Important,
    Priority::Urgent => MessagePriority::Urgent,
    Priority::Normal | Priority::Unknown => MessagePriority::Normal,
  }
}

fn refusal_error(refusal: ScheduleRefusal) -> CoreError {
  CoreError::ScheduleRefused(refusal)
}

/// The time an enqueued message shows as sent until the server numbers it.
struct FrozenClock(i64);

impl Clock for FrozenClock {
  fn now_ms(&self) -> i64 {
    self.0
  }
}
