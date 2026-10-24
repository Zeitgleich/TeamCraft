//! Catching up every conversation after a fresh gateway session (issue #247): the conversation the
//! user looks at first, the network round trips of several conversations at once (up to
//! [`super::ClientSettings::catch_up_concurrency`]), and a mark on each conversation still being
//! caught up, so the UI shows "catching up" there instead of a list that looks complete.
//!
//! The MLS state is one per device, so the groups are still processed one after the other; what
//! runs side by side is waiting for the server: member lists, and the first page of each
//! conversation's MLS queue and messages.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures_util::future::join_all;
use teamcraft_proto::{
  ConversationId, Id, ListConversationMembersResponse, ListMessagesQuery, ListMessagesResponse,
  ListMlsMessagesQuery, ListMlsMessagesResponse, Snowflake,
};

use super::Client;
use super::sync::ResyncPart;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageFilter, Table};

/// Key prefix of the "still catching up" marks in [`Table::SyncState`], one per conversation.
pub(crate) const CATCHING_UP_PREFIX: &str = "catching_up/";

/// The conversation the user looks at, so a catch-up takes it next. The host sets it from the
/// UI's reads while a sync holds the client; clones share it.
#[derive(Clone, Debug)]
pub struct CatchUpFocus(Arc<AtomicI64>);

/// What [`CatchUpFocus`] holds before a conversation was set (snowflakes are never negative).
const NO_FOCUS: i64 = -1;

/// The first page of a conversation's MLS queue and messages, read ahead.
#[derive(Default)]
pub(crate) struct FirstPages {
  pub(crate) maybe_mls: Option<Result<ListMlsMessagesResponse, ApiError>>,
  pub(crate) maybe_messages: Option<Result<ListMessagesResponse, ApiError>>,
}

impl Default for CatchUpFocus {
  fn default() -> Self {
    Self(Arc::new(AtomicI64::new(NO_FOCUS)))
  }
}

impl CatchUpFocus {
  pub fn set(&self, conversation: ConversationId) {
    self.0.store(conversation.snowflake().get(), Ordering::Relaxed);
  }

  pub fn get(&self) -> Option<ConversationId> {
    Snowflake::new(self.0.load(Ordering::Relaxed))
      .ok()
      .map(ConversationId::from_snowflake)
  }
}

/// Whether the conversation is still being caught up.
pub fn is_catching_up(store: &impl LocalStore, conversation: ConversationId) -> Result<bool, CoreError> {
  Ok(store.get(Table::SyncState, &marker_key(conversation))?.is_some())
}

/// Drops every mark, e.g. those of a catch-up the app was closed during.
pub(crate) fn clear_marks(store: &impl LocalStore) -> Result<(), CoreError> {
  for (key, _) in store.list(Table::SyncState)? {
    if key.starts_with(CATCHING_UP_PREFIX) {
      store.delete(Table::SyncState, &key)?;
    }
  }
  Ok(())
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The handle the host uses to say which conversation the user looks at.
  pub fn catch_up_focus(&self) -> CatchUpFocus {
    self.catch_up_focus.clone()
  }

  /// Catches up the stored conversations: member lists (in `members_order`), welcomes, groups to
  /// join, then each one's MLS queue and messages, the focused one first, the round trips of up to
  /// `catch_up_concurrency` at once. Every conversation is marked from the start until it is done;
  /// a failure clears the marks (the connection fails and is retried, the UI shows that).
  pub(crate) async fn catch_up_conversations(
    &mut self,
    conversations: &[ConversationId],
    members_order: &[ConversationId],
  ) -> Result<(), CoreError> {
    for conversation in conversations {
      records::put_json(&self.store, Table::SyncState, &marker_key(*conversation), &true)?;
    }
    let outcome = self.catch_up_marked(conversations, members_order).await;
    if outcome.is_err() {
      clear_marks(&self.store)?;
    }
    outcome
  }

  async fn catch_up_marked(
    &mut self,
    conversations: &[ConversationId],
    members_order: &[ConversationId],
  ) -> Result<(), CoreError> {
    self.refresh_members_of(members_order).await?;
    // Welcomes in the server's order: their cursor only moves forward.
    self.sync_welcomes().await?;
    // After the welcomes: a member's add (with a Welcome) wins over claiming the place ourselves.
    self.join_missing_groups().await?;
    self.catch_up_each(conversations.to_vec()).await
  }

  async fn catch_up_each(&mut self, mut waiting: Vec<ConversationId>) -> Result<(), CoreError> {
    while !waiting.is_empty() {
      let batch = next_batch(
        &mut waiting,
        self.catch_up_focus.get(),
        self.settings.catch_up_concurrency,
      );
      let pages = self.read_first_pages(&batch).await?;
      for (conversation, first) in batch.into_iter().zip(pages) {
        // One conversation's failure (a 403 right after a removal) marks it for another try and
        // the catch-up goes on with the others (issue #159).
        let result = self.catch_up_content_from(conversation, first).await;
        if self.isolate(conversation, ResyncPart::Content, result)? {
          self.unmark_resync(conversation, ResyncPart::Content)?;
        }
        self.store.delete(Table::SyncState, &marker_key(conversation))?;
      }
    }
    Ok(())
  }

  /// One conversation's part of a catch-up, with its first pages possibly read ahead: its MLS
  /// backlog, new messages, messages waiting for keys and the senders' keys.
  pub(super) async fn catch_up_content_from(
    &mut self,
    conversation: ConversationId,
    first: FirstPages,
  ) -> Result<(), CoreError> {
    if self.has_group(conversation)? {
      self.sync_mls_from(conversation, first.maybe_mls).await?;
    }
    self
      .catch_up_messages_from(conversation, first.maybe_messages)
      .await?;
    self.retry_undecrypted(conversation)?;
    // Senders this device has no verified key for yet (it isn't in the group, or just joined).
    self.learn_sender_keys(conversation).await
  }

  /// Reads the first page of each conversation's MLS queue and messages at once. Failures are
  /// kept for the conversation's own catch-up to handle, like a failed read of its own.
  async fn read_first_pages(&mut self, batch: &[ConversationId]) -> Result<Vec<FirstPages>, CoreError> {
    let mut queries = Vec::with_capacity(batch.len());
    for conversation in batch {
      let maybe_mls = (self.has_group(*conversation)? && !self.is_unfinished_dm_founding(*conversation)?)
        .then(|| ListMlsMessagesQuery {
          maybe_after: self.sync.mls_cursors.get(conversation).copied(),
          maybe_limit: None,
        });
      queries.push((
        *conversation,
        maybe_mls,
        self.first_messages_query(*conversation)?,
      ));
    }
    let api = &self.api;
    let reads = queries
      .iter()
      .map(|(conversation, maybe_mls, messages)| async move {
        let maybe_mls = match maybe_mls {
          Some(query) => Some(api.list_mls_messages(*conversation, query).await),
          None => None,
        };
        FirstPages {
          maybe_mls,
          maybe_messages: Some(api.list_messages(*conversation, messages).await),
        }
      });
    Ok(join_all(reads).await)
  }

  /// The first request of [`Self::catch_up_messages`]: the first time a page of history (before
  /// the oldest message stored, the newest page when there is none), otherwise what is newer than
  /// the newest message stored.
  pub(crate) fn first_messages_query(
    &self,
    conversation: ConversationId,
  ) -> Result<ListMessagesQuery, CoreError> {
    let server_ids = self
      .store
      .messages(conversation, MessageFilter::All)?
      .into_iter()
      .filter_map(|message| message.maybe_server_id);
    let (maybe_before, maybe_after) = if self.is_history_loaded(conversation)? {
      (None, server_ids.max())
    } else {
      (server_ids.min(), None)
    };
    Ok(ListMessagesQuery {
      maybe_before,
      maybe_after,
      maybe_limit: Some(self.settings.history_page_size),
    })
  }

  /// Member lists of the conversations, up to `catch_up_concurrency` requests at once, stored in
  /// the given order.
  pub(crate) async fn refresh_members_of(
    &mut self,
    conversations: &[ConversationId],
  ) -> Result<(), CoreError> {
    for batch in conversations.chunks(self.settings.catch_up_concurrency.max(1)) {
      let reads = batch
        .iter()
        .map(|conversation| self.api.list_members(*conversation));
      let lists: Vec<Result<ListConversationMembersResponse, ApiError>> = join_all(reads).await;
      for (conversation, list) in batch.iter().zip(lists) {
        let result = self.store_members(*conversation, list);
        if self.isolate(*conversation, ResyncPart::Members, result)? {
          self.unmark_resync(*conversation, ResyncPart::Members)?;
        }
      }
    }
    Ok(())
  }
}

/// The next conversations to catch up, at most `size`: the focused one first, if it is still
/// waiting, then the others in their order.
fn next_batch(
  waiting: &mut Vec<ConversationId>,
  maybe_focus: Option<ConversationId>,
  size: usize,
) -> Vec<ConversationId> {
  if let Some(focus) = maybe_focus
    && let Some(index) = waiting.iter().position(|conversation| *conversation == focus)
  {
    let focused = waiting.remove(index);
    waiting.insert(0, focused);
  }
  let size = size.max(1).min(waiting.len());
  waiting.drain(..size).collect()
}

fn marker_key(conversation: ConversationId) -> String {
  format!("{CATCHING_UP_PREFIX}{conversation}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_focus_round_trips() {
    let focus = CatchUpFocus::default();
    assert_eq!(focus.get(), None);
    focus.clone().set(id(42));
    assert_eq!(focus.get(), Some(id(42)));
  }

  #[test]
  fn the_focused_conversation_comes_first_then_the_order() {
    let mut waiting: Vec<ConversationId> = (1..=5).map(id).collect();
    assert_eq!(next_batch(&mut waiting, Some(id(4)), 2), vec![id(4), id(1)]);
    assert_eq!(next_batch(&mut waiting, Some(id(4)), 2), vec![id(2), id(3)]);
    assert_eq!(next_batch(&mut waiting, None, 2), vec![id(5)]);
    assert!(waiting.is_empty());
  }

  #[test]
  fn a_batch_has_at_least_one_conversation() {
    let mut waiting = vec![id(1), id(2)];
    assert_eq!(next_batch(&mut waiting, None, 0), vec![id(1)]);
  }

  fn id(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
