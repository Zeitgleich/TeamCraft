//! Reading further back when the reader scrolls past what this device holds. The feeds show the
//! store, and a store holds what the first sync and the imports read: the browser the newest page
//! only, phones and desktops three months. What lies before comes one page per request of the UI
//! ([`crate::ui_backend::CoreCommand::LoadOlderHistory`]) and stays in the store.
//!
//! One small record per conversation in [`Table::SyncState`] says where the reading stands and how
//! the last request ended. It goes there, not to the host, because the UI reads it like any other
//! state, on every platform: a failure is shown at the top of the list and asks again by itself on
//! the next connection, and the beginning, once reached, costs no request after a restart.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, ListMessagesQuery, MessageId};

use super::Client;
use super::imported_history::PagingPosition;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageEdge, Table};

/// Counts the gateway connections, so that a failure can tell it is from an earlier one.
const CONNECTIONS_KEY: &str = "older_history_connections";

/// Where reading back through a conversation stands, under [`record_key`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OlderHistoryRecord {
  maybe_position: Option<PagingPosition>,
  /// The request answered last, also when it failed.
  pub(crate) answered: u64,
  /// The connection during which that request failed; `None` when it didn't.
  pub(crate) maybe_failed_in: Option<u64>,
}

impl OlderHistoryRecord {
  #[cfg(feature = "ui")]
  pub(crate) fn is_at_beginning(&self) -> bool {
    self.maybe_position == Some(PagingPosition::Beginning)
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Answers request `request` of the UI with one page before everything this device has. A
  /// failure is the answer: the UI shows it where the page would have appeared. Only a refused
  /// token also comes back as an error, for the host that refreshes it and runs the command again.
  pub async fn load_older_history(
    &mut self,
    conversation: ConversationId,
    request: u64,
  ) -> Result<(), CoreError> {
    let key = record_key(conversation);
    let record = records::get_json::<OlderHistoryRecord>(&self.store, Table::SyncState, &key)?;
    let maybe_position = record.and_then(|record| record.maybe_position);
    let outcome = self.read_page_before_held(conversation, maybe_position).await;
    let answer = match &outcome {
      Ok(position) => OlderHistoryRecord {
        maybe_position: Some(*position),
        answered: request,
        maybe_failed_in: None,
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not read older history");
        OlderHistoryRecord {
          maybe_position,
          answered: request,
          maybe_failed_in: Some(read_connections(&self.store)?),
        }
      },
    };
    records::put_json(&self.store, Table::SyncState, &key, &answer)?;
    match outcome {
      Err(error @ CoreError::Api(ApiError::Status { status: 401, .. })) => Err(error),
      _ => Ok(()),
    }
  }

  /// Reads the page and stores it; returns where the next one starts.
  async fn read_page_before_held(
    &mut self,
    conversation: ConversationId,
    maybe_position: Option<PagingPosition>,
  ) -> Result<PagingPosition, CoreError> {
    let maybe_read_to = match maybe_position {
      Some(PagingPosition::Before(before)) => Some(before),
      // Asked again after the beginning: what an import or a new grant may have put before it.
      Some(PagingPosition::Beginning) | None => None,
    };
    let maybe_oldest = self.store.edge_server_id(conversation, MessageEdge::Oldest)?;
    let query = ListMessagesQuery {
      maybe_before: [maybe_oldest, maybe_read_to].into_iter().flatten().min(),
      maybe_after: None,
      maybe_limit: Some(self.settings.history_page_size),
    };
    let page = self.api.list_messages(conversation, &query).await?;
    // The page's own oldest message is the next cursor, not the store's: a message parked for its
    // key isn't stored yet, and the same page would be read again.
    let maybe_page_oldest = page.messages.iter().map(|message| message.id).min();
    let position = next_position(maybe_page_oldest, page.has_more);
    self.receive_page(page.messages)?;
    Ok(position)
  }

  /// A new connection: failures from before it no longer stand, so a reader at the top of the
  /// list gets the page without doing anything.
  pub(super) fn note_older_history_connection(&self) -> Result<(), CoreError> {
    let next = read_connections(&self.store)?.wrapping_add(1);
    records::put_json(&self.store, Table::SyncState, CONNECTIONS_KEY, &next)?;
    Ok(())
  }

  /// What is known of a conversation that is gone says nothing about it if it comes back: the
  /// history this user may see can differ then.
  pub(super) fn forget_older_history(&self, conversation: ConversationId) -> Result<(), CoreError> {
    self.store.delete(Table::SyncState, &record_key(conversation))?;
    Ok(())
  }
}

/// The record of `conversation`, for the UI.
#[cfg(feature = "ui")]
pub(crate) fn older_history_record(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Option<OlderHistoryRecord>, CoreError> {
  records::get_json(store, Table::SyncState, &record_key(conversation))
}

/// Whether a failure in connection `failed_in` still stands.
#[cfg(feature = "ui")]
pub(crate) fn is_failure_current(store: &impl LocalStore, failed_in: u64) -> Result<bool, CoreError> {
  Ok(read_connections(store)? == failed_in)
}

fn next_position(maybe_page_oldest: Option<MessageId>, has_more: bool) -> PagingPosition {
  match maybe_page_oldest {
    Some(oldest) if has_more => PagingPosition::Before(oldest),
    _ => PagingPosition::Beginning,
  }
}

fn read_connections(store: &impl LocalStore) -> Result<u64, CoreError> {
  Ok(records::get_json::<u64>(store, Table::SyncState, CONNECTIONS_KEY)?.unwrap_or(0))
}

fn record_key(conversation: ConversationId) -> String {
  format!("older_history/{conversation}")
}
