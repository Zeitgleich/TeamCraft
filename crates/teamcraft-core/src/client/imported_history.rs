//! Reading back through imported history (e2ee.md §4.10). An import into a conversation this
//! device already held places its messages before everything the device has, and the feed shows
//! only what the store holds: scrolling up never reads the server. So once an import finished,
//! its history is read page by page in the background, one page per upkeep tick, back to the
//! beginning. Where each conversation stands is kept in the sync state, so the paging resumes
//! after a restart, and a later import pages again.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, ListMessagesQuery, MessageId};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageEdge, Table};

/// Where reading back through a conversation's imported history stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ImportPaging {
  /// The import it reads: the conversation's `maybe_imported_through` when it started.
  through: MessageId,
  position: PagingPosition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PagingPosition {
  /// The messages before this one are still to be read.
  Before(MessageId),
  /// The beginning (or the history this device may see) is reached.
  Beginning,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Reads one page of imported history this device doesn't hold yet, of the first conversation
  /// that has some left. Returns how many messages arrived. Without anything left it is a no-op
  /// that reads nothing, not even the store, until the next import finishes.
  pub async fn page_imported_history(&mut self) -> Result<usize, CoreError> {
    if self.is_imported_history_paged {
      return Ok(0);
    }
    let Some((conversation, through, maybe_before)) = self.next_imported_page()? else {
      self.is_imported_history_paged = true;
      return Ok(0);
    };
    let query = ListMessagesQuery {
      maybe_before,
      maybe_after: None,
      maybe_limit: Some(self.settings.imported_page_size),
    };
    let (count, position) = match self.api.list_messages(conversation, &query).await {
      Ok(page) => {
        let count = page.messages.len();
        // The page's own oldest message is the next cursor, not the store's: a message parked for
        // its key isn't stored yet, and the same page would be read again.
        let position = match page.messages.iter().map(|message| message.id).min() {
          Some(oldest) if page.has_more => PagingPosition::Before(oldest),
          _ => PagingPosition::Beginning,
        };
        self.receive_page(page.messages)?;
        (count, position)
      },
      Err(error) if error.is_retry_later() => return Err(error.into()),
      // Refused for good (left meanwhile, no longer allowed): asking again every tick would only
      // hold up the other conversations.
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "stopped reading imported history");
        (0, PagingPosition::Beginning)
      },
    };
    records::put_json(
      &self.store,
      Table::SyncState,
      &paging_key(conversation),
      &ImportPaging { through, position },
    )?;
    if position == PagingPosition::Beginning {
      tracing::info!(conversation_id = %conversation, "read all imported history");
    }
    Ok(count)
  }

  /// The conversation whose imported history is read next, its import and the cursor of the next
  /// page. A conversation's first page of history comes first (the catch-up reads it).
  fn next_imported_page(&self) -> Result<Option<(ConversationId, MessageId, Option<MessageId>)>, CoreError> {
    for (conversation, response) in self.cached_conversations()? {
      let Some(through) = response.maybe_imported_through else {
        continue;
      };
      if !self.is_history_loaded(conversation)? {
        continue;
      }
      let maybe_paging =
        records::get_json::<ImportPaging>(&self.store, Table::SyncState, &paging_key(conversation))?;
      match maybe_paging {
        Some(paging) if paging.through == through => match paging.position {
          PagingPosition::Before(before) => return Ok(Some((conversation, through, Some(before)))),
          PagingPosition::Beginning => {},
        },
        // Not started, or started for an earlier import: this one lies before all the device has.
        _ => {
          let maybe_oldest = self.store.edge_server_id(conversation, MessageEdge::Oldest)?;
          return Ok(Some((conversation, through, maybe_oldest)));
        },
      }
    }
    Ok(None)
  }
}

/// Marks in [`Table::SyncState`] where reading back through a conversation's imported history
/// stands ([`ImportPaging`]).
fn paging_key(conversation: ConversationId) -> String {
  format!("imported_paging/{conversation}")
}
