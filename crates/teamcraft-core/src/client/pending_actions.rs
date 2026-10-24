//! Small actions that must reach the server even if the device is offline when the user takes them
//! (issue #159): read positions, acknowledgements, the server's pin flag and the manual presence.
//!
//! Each action is stored first and sent afterwards: right away, on every upkeep tick, and at the
//! end of a full sync, until the server took it. The queue survives a restart (it lives in the
//! store) and coalesces: one entry per target, so marking a busy channel read a hundred times while
//! offline sends one request. The entry's key is the action's idempotency key; every action is a
//! "set to" on the server (`GREATEST` for read positions, `ON CONFLICT DO NOTHING` for
//! acknowledgements, a flag for pins, a value for presence), so a retry after a lost response
//! changes nothing. A read position in the queue only ever moves forward.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ClientMessageId, ConversationId, MessageId, SetPresenceRequest, SetReadPositionRequest,
};

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Where the queue is stored (one record in [`Table::SyncState`]).
const QUEUE_KEY: &str = "pending_actions";
/// Entries kept at most; beyond it the oldest goes. Coalescing keeps real queues far below.
const MAX_PENDING_ACTIONS: usize = 1000;

/// An action waiting for the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingAction {
  /// The user read the conversation up to `message`.
  ReadPosition {
    conversation: ConversationId,
    message: MessageId,
  },
  /// The user confirmed an urgent message.
  Acknowledge {
    conversation: ConversationId,
    message: MessageId,
  },
  /// The server's pin flag of a message (keeps it through retention); the encrypted pin itself
  /// goes through the outbox.
  ServerPin {
    conversation: ConversationId,
    target: ClientMessageId,
    is_pinned: bool,
  },
  /// The user's manual presence (do not disturb, invisible, automatic).
  Presence { request: SetPresenceRequest },
}

/// The stored queue: actions by their key.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PendingActions {
  entries: BTreeMap<String, QueuedAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct QueuedAction {
  action: PendingAction,
  /// When the user took it (this device's clock; only used to drop the oldest when full).
  queued_at_ms: i64,
}

/// What became of one attempt.
enum Attempt {
  Sent,
  /// Not possible yet (a pin of a message the server has no ID for); stays queued.
  Waiting,
  /// The server refused it for good (the conversation is gone, the message unknown): dropped.
  Refused(ApiError),
  /// Offline, server trouble, a token to renew or an unreadable store: stays queued, the rest
  /// waits too.
  RetryLater(CoreError),
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Queues the action and tries to send what is queued. Never fails for being offline: the action
  /// is stored and goes out later.
  pub async fn queue_action(&mut self, action: PendingAction) -> Result<(), CoreError> {
    self.store_action(action)?;
    self.flush_pending_actions().await?;
    Ok(())
  }

  /// Sends the queued actions in order. Stops at the first one that has to wait for the network;
  /// drops those the server refuses for good. Returns how many were sent.
  pub async fn flush_pending_actions(&mut self) -> Result<usize, CoreError> {
    let mut queue = load_queue(&self.store)?;
    if queue.entries.is_empty() {
      return Ok(0);
    }
    let mut sent = 0;
    let keys: Vec<String> = queue.entries.keys().cloned().collect();
    for key in keys {
      let Some(queued) = queue.entries.get(&key).cloned() else {
        continue;
      };
      match self.attempt(&queued.action).await {
        Attempt::Sent => {
          queue.entries.remove(&key);
          sent += 1;
          self.after_sent(&queued.action).await;
        },
        Attempt::Waiting => {},
        Attempt::Refused(error) => {
          tracing::warn!(%error, kind = queued.action.kind(), "the server refused a queued action; dropped");
          queue.entries.remove(&key);
        },
        Attempt::RetryLater(error) => {
          tracing::debug!(%error, "queued actions wait for the server");
          break;
        },
      }
    }
    save_queue(&self.store, &queue)?;
    Ok(sent)
  }

  /// How many actions wait for the server.
  pub fn pending_action_count(&self) -> Result<usize, CoreError> {
    Ok(load_queue(&self.store)?.entries.len())
  }

  fn store_action(&self, action: PendingAction) -> Result<(), CoreError> {
    let mut queue = load_queue(&self.store)?;
    let key = action.key();
    let action = match (queue.entries.get(&key).map(|queued| &queued.action), action) {
      // A read position never moves back, also not when an older one is queued after a newer one.
      (
        Some(PendingAction::ReadPosition { message: queued, .. }),
        PendingAction::ReadPosition {
          conversation,
          message,
        },
      ) => PendingAction::ReadPosition {
        conversation,
        message: message.max(*queued),
      },
      (_, action) => action,
    };
    queue.entries.insert(
      key,
      QueuedAction {
        action,
        queued_at_ms: self.now_ms(),
      },
    );
    while queue.entries.len() > MAX_PENDING_ACTIONS {
      let Some(oldest) = queue
        .entries
        .iter()
        .min_by_key(|(_, queued)| queued.queued_at_ms)
        .map(|(key, _)| key.clone())
      else {
        break;
      };
      tracing::warn!("too many queued actions; the oldest is dropped");
      queue.entries.remove(&oldest);
    }
    save_queue(&self.store, &queue)
  }

  async fn attempt(&mut self, action: &PendingAction) -> Attempt {
    let result = match action {
      PendingAction::ReadPosition {
        conversation,
        message,
      } => {
        let request = SetReadPositionRequest { message_id: *message };
        self.api.set_read_position(*conversation, &request).await
      },
      PendingAction::Acknowledge {
        conversation,
        message,
      } => self.api.acknowledge_message(*conversation, *message).await,
      PendingAction::ServerPin {
        conversation,
        target,
        is_pinned,
      } => match self.store.message(*conversation, *target) {
        Ok(Some(stored)) => match stored.maybe_server_id {
          Some(message) => {
            self
              .api
              .set_message_pinned(*conversation, message, *is_pinned)
              .await
          },
          // Still in the outbox: the flag follows once the message has its server ID.
          None => return Attempt::Waiting,
        },
        // The message is gone from this device (deleted, conversation left): nothing to flag.
        Ok(None) => return Attempt::Sent,
        Err(error) => return Attempt::RetryLater(error.into()),
      },
      PendingAction::Presence { request } => self.api.set_presence(request).await,
    };
    match result {
      Ok(()) => Attempt::Sent,
      Err(error) if error.is_retry_later() => Attempt::RetryLater(CoreError::Api(error)),
      Err(error) => Attempt::Refused(error),
    }
  }

  /// What follows a sent action. Failures here are not the action's: logged only.
  async fn after_sent(&mut self, action: &PendingAction) {
    if let PendingAction::ReadPosition { conversation, .. } = action
      && let Err(error) = self.refresh_conversation(*conversation).await
    {
      // The `unread_update` event brings the server's counts as well.
      tracing::debug!(%error, "conversation not refreshed after its read position");
    }
  }
}

impl PendingAction {
  /// The target the action is about: a newer action for the same target replaces the older one.
  fn key(&self) -> String {
    match self {
      Self::ReadPosition { conversation, .. } => format!("read/{conversation}"),
      Self::Acknowledge {
        conversation,
        message,
      } => format!("ack/{conversation}/{message}"),
      Self::ServerPin {
        conversation, target, ..
      } => format!("pin/{conversation}/{target}"),
      Self::Presence { .. } => "presence".to_owned(),
    }
  }

  /// For logs: never the content, only what kind of action it was.
  fn kind(&self) -> &'static str {
    match self {
      Self::ReadPosition { .. } => "read_position",
      Self::Acknowledge { .. } => "acknowledge",
      Self::ServerPin { .. } => "server_pin",
      Self::Presence { .. } => "presence",
    }
  }
}

fn load_queue(store: &impl LocalStore) -> Result<PendingActions, CoreError> {
  Ok(records::get_json(store, Table::SyncState, QUEUE_KEY)?.unwrap_or_default())
}

fn save_queue(store: &impl LocalStore, queue: &PendingActions) -> Result<(), CoreError> {
  records::put_json(store, Table::SyncState, QUEUE_KEY, queue)
}
