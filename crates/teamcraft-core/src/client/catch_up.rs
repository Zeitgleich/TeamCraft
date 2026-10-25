//! Catching up every conversation after a fresh gateway session (issue #247): the conversation the
//! user looks at first, the network round trips of several conversations at once (up to
//! [`super::ClientSettings::catch_up_concurrency`]), and a mark on each conversation still being
//! caught up, so the UI shows "catching up" there instead of a list that looks complete.
//!
//! The conversation the user looks at doesn't wait for the others: as soon as the host names it
//! ([`CatchUpFocus`]), the catch-up takes it completely (members, its welcomes, its MLS queue and
//! messages) between two steps of the rest, so it is readable within seconds also when every
//! member list and 300 welcomes are still to come.
//!
//! The MLS state is one per device, so the groups are still processed one after the other; what
//! runs side by side is waiting for the server: member lists, and the first page of each
//! conversation's MLS queue and messages.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use futures_util::future::join_all;
use futures_util::task::AtomicWaker;
use teamcraft_proto::{
  ConversationId, Id, ListConversationMembersResponse, ListMessagesQuery, ListMessagesResponse,
  ListMlsMessagesQuery, ListMlsMessagesResponse, MlsMessageResponse, Snowflake,
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
/// UI's reads while a sync holds the client; clones share it. It also counts how often the focused
/// conversation was caught up, so the host can show it at once.
#[derive(Clone, Debug)]
pub struct CatchUpFocus {
  conversation: Arc<AtomicI64>,
  caught_up: Arc<AtomicU64>,
  /// The host waiting in [`CatchUpFocus::caught_up_since`] (one waiter).
  waker: Arc<AtomicWaker>,
}

/// Welcomes a catch-up keeps after the focus first read them, for the next focus; with more waiting,
/// each focus reads them again.
const MAX_KEPT_WELCOMES: usize = 1_000;

/// What [`CatchUpFocus`] holds before a conversation was set (snowflakes are never negative).
const NO_FOCUS: i64 = -1;

/// The conversations of one catch-up and those the focus took ahead of the rest.
struct AheadOfTheRest {
  conversations: BTreeSet<ConversationId>,
  /// Taken ahead once each, whatever came of it.
  tried: BTreeSet<ConversationId>,
  /// Completely caught up ahead; the rest of the catch-up leaves them out.
  done: BTreeSet<ConversationId>,
  /// The welcomes waiting when the focus first needed them ([`MAX_KEPT_WELCOMES`]). One arriving
  /// later is joined by the in-order pass.
  maybe_welcomes: Option<Vec<MlsMessageResponse>>,
}

/// The first page of a conversation's MLS queue and messages, read ahead.
#[derive(Default)]
pub(crate) struct FirstPages {
  pub(crate) maybe_mls: Option<Result<ListMlsMessagesResponse, ApiError>>,
  pub(crate) maybe_messages: Option<Result<ListMessagesResponse, ApiError>>,
}

impl Default for CatchUpFocus {
  fn default() -> Self {
    Self {
      conversation: Arc::new(AtomicI64::new(NO_FOCUS)),
      caught_up: Arc::new(AtomicU64::new(0)),
      waker: Arc::new(AtomicWaker::new()),
    }
  }
}

impl CatchUpFocus {
  pub fn set(&self, conversation: ConversationId) {
    self
      .conversation
      .store(conversation.snowflake().get(), Ordering::Relaxed);
  }

  pub fn get(&self) -> Option<ConversationId> {
    Snowflake::new(self.conversation.load(Ordering::Relaxed))
      .ok()
      .map(ConversationId::from_snowflake)
  }

  /// Grows whenever a catch-up finished the focused conversation.
  pub fn caught_up_count(&self) -> u64 {
    self.caught_up.load(Ordering::Relaxed)
  }

  /// Completes with the count once it differs from `seen`: no timer, the catch-up wakes the host.
  pub fn caught_up_since(&self, seen: u64) -> impl std::future::Future<Output = u64> + '_ {
    std::future::poll_fn(move |context| {
      self.waker.register(context.waker());
      match self.caught_up_count() {
        count if count != seen => std::task::Poll::Ready(count),
        _ => std::task::Poll::Pending,
      }
    })
  }

  fn note_caught_up(&self) {
    self.caught_up.fetch_add(1, Ordering::Relaxed);
    self.waker.wake();
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

  /// Takes the host's handle instead of its own, so the host can name the focus without the client
  /// (a desktop command while the core thread is busy syncing).
  pub fn share_catch_up_focus(&mut self, focus: CatchUpFocus) {
    self.catch_up_focus = focus;
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
    // A catch-up dropped while it held the cursor (the app went away) must not keep it held.
    self.is_welcome_cursor_held = false;
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
    let mut phases = PhaseTimer::start(self.now_ms());
    let mut ahead = AheadOfTheRest {
      conversations: conversations.iter().copied().collect(),
      tried: BTreeSet::new(),
      done: BTreeSet::new(),
      maybe_welcomes: None,
    };
    self
      .refresh_members_around_focus(members_order, &mut ahead, &phases)
      .await?;
    phases.lap("member lists", self.now_ms());
    self.sync_welcomes_around_focus(&mut ahead, &phases).await?;
    phases.lap("welcomes", self.now_ms());
    // After the welcomes: a member's add (with a Welcome) wins over claiming the place ourselves.
    self.join_missing_groups().await?;
    phases.lap("missing groups", self.now_ms());
    let waiting = conversations
      .iter()
      .copied()
      .filter(|conversation| !ahead.done.contains(conversation))
      .collect();
    self.catch_up_each(waiting, &phases).await?;
    phases.lap("content", self.now_ms());
    tracing::info!(
      conversations = conversations.len(),
      total_ms = phases.total_ms(),
      mls_storage_bytes = records::mls_storage_len(&self.provider),
      "catch-up done"
    );
    Ok(())
  }

  async fn catch_up_each(
    &mut self,
    mut waiting: Vec<ConversationId>,
    phases: &PhaseTimer,
  ) -> Result<(), CoreError> {
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
        if self.catch_up_focus.get() == Some(conversation) {
          self.catch_up_focus.note_caught_up();
          tracing::info!(
            ms = self.now_ms().saturating_sub(phases.started_ms),
            "catch-up: the focused conversation is caught up"
          );
        }
      }
    }
    Ok(())
  }

  /// Member lists like [`Self::refresh_members_of`], the focused conversation taken ahead between
  /// two batches.
  async fn refresh_members_around_focus(
    &mut self,
    members_order: &[ConversationId],
    ahead: &mut AheadOfTheRest,
    phases: &PhaseTimer,
  ) -> Result<(), CoreError> {
    let batch_size = self.settings.catch_up_concurrency.max(1);
    let mut rest = members_order.iter().copied();
    loop {
      self.catch_up_focus_ahead(ahead, phases).await?;
      let batch: Vec<ConversationId> = rest
        .by_ref()
        .filter(|conversation| !ahead.tried.contains(conversation))
        .take(batch_size)
        .collect();
      if batch.is_empty() {
        return Ok(());
      }
      self.refresh_members_of(&batch).await?;
    }
  }

  /// [`Self::sync_welcomes`] in the server's order (their cursor only moves forward), the focused
  /// conversation taken ahead between two welcomes.
  async fn sync_welcomes_around_focus(
    &mut self,
    ahead: &mut AheadOfTheRest,
    phases: &PhaseTimer,
  ) -> Result<(), CoreError> {
    // As in `sync_welcomes`: spent key packages wait until every welcome was read (#395).
    self.are_welcomes_synced = false;
    loop {
      let query = ListMlsMessagesQuery {
        maybe_after: self.sync.maybe_welcome_cursor,
        maybe_limit: None,
      };
      let page = self.api.list_welcomes(self.account.device_id, &query).await?;
      for welcome in &page.messages {
        self.catch_up_focus_ahead(ahead, phases).await?;
        self.join_from_welcome(welcome).await?;
      }
      if !page.has_more || page.messages.is_empty() {
        self.are_welcomes_synced = true;
        return Ok(());
      }
    }
  }

  /// Catches up the focused conversation completely, ahead of the rest, once per catch-up: its
  /// member list, its welcomes (out of the server's order, without moving the welcome cursor, so
  /// the in-order pass still meets the welcomes before them), its MLS queue and messages. Without
  /// a group after its welcomes, it waits for the rest of the catch-up (claiming a place comes
  /// after all welcomes).
  async fn catch_up_focus_ahead(
    &mut self,
    ahead: &mut AheadOfTheRest,
    phases: &PhaseTimer,
  ) -> Result<(), CoreError> {
    let Some(focus) = self.catch_up_focus.get() else {
      return Ok(());
    };
    if !ahead.conversations.contains(&focus) || !ahead.tried.insert(focus) {
      return Ok(());
    }
    let read = self.api.list_members(focus).await;
    let result = self.store_members(focus, read);
    if self.isolate(focus, ResyncPart::Members, result)? {
      self.unmark_resync(focus, ResyncPart::Members)?;
    }
    if !self.has_group(focus)? {
      self.join_welcomes_of(focus, ahead).await?;
    }
    if !self.has_group(focus)? {
      return Ok(());
    }
    let result = self.catch_up_content_from(focus, FirstPages::default()).await;
    if self.isolate(focus, ResyncPart::Content, result)? {
      self.unmark_resync(focus, ResyncPart::Content)?;
    }
    self.store.delete(Table::SyncState, &marker_key(focus))?;
    ahead.done.insert(focus);
    self.catch_up_focus.note_caught_up();
    tracing::info!(
      ms = self.now_ms().saturating_sub(phases.started_ms),
      "catch-up: the focused conversation is caught up ahead of the rest"
    );
    Ok(())
  }

  /// Joins the conversation's waiting welcomes, in their order, with the welcome cursor held.
  async fn join_welcomes_of(
    &mut self,
    conversation: ConversationId,
    ahead: &mut AheadOfTheRest,
  ) -> Result<(), CoreError> {
    let welcomes = match &ahead.maybe_welcomes {
      Some(kept) => kept
        .iter()
        .filter(|welcome| welcome.conversation_id == conversation)
        .cloned()
        .collect(),
      None => {
        let (welcomes, maybe_all) = self.read_waiting_welcomes(conversation).await?;
        ahead.maybe_welcomes = maybe_all;
        welcomes
      },
    };
    for welcome in &welcomes {
      self.is_welcome_cursor_held = true;
      let joined = self.join_from_welcome(welcome).await;
      self.is_welcome_cursor_held = false;
      joined?;
    }
    Ok(())
  }

  /// The waiting welcomes of `conversation`, and all waiting welcomes if there are at most
  /// [`MAX_KEPT_WELCOMES`].
  async fn read_waiting_welcomes(
    &self,
    conversation: ConversationId,
  ) -> Result<(Vec<MlsMessageResponse>, Option<Vec<MlsMessageResponse>>), CoreError> {
    let mut maybe_after = self.sync.maybe_welcome_cursor;
    let mut welcomes = Vec::new();
    let mut maybe_all = Some(Vec::new());
    loop {
      let query = ListMlsMessagesQuery {
        maybe_after,
        maybe_limit: None,
      };
      let page = self.api.list_welcomes(self.account.device_id, &query).await?;
      maybe_after = page.messages.last().map(|last| last.id);
      let is_last = !page.has_more || page.messages.is_empty();
      welcomes.extend(
        page
          .messages
          .iter()
          .filter(|welcome| welcome.conversation_id == conversation)
          .cloned(),
      );
      if let Some(all) = &mut maybe_all {
        all.extend(page.messages);
        if all.len() > MAX_KEPT_WELCOMES {
          maybe_all = None;
        }
      }
      if is_last {
        return Ok((welcomes, maybe_all));
      }
    }
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

/// Logs how long each phase of a catch-up took (docs/performance.md: where the time to a readable
/// channel goes on a large workspace). Client clock; only durations, nothing about the content.
struct PhaseTimer {
  started_ms: i64,
  lap_ms: i64,
}

impl PhaseTimer {
  fn start(now_ms: i64) -> Self {
    Self {
      started_ms: now_ms,
      lap_ms: now_ms,
    }
  }

  fn lap(&mut self, phase: &'static str, now_ms: i64) {
    tracing::info!(
      phase,
      ms = now_ms.saturating_sub(self.lap_ms),
      "catch-up phase done"
    );
    self.lap_ms = now_ms;
  }

  fn total_ms(&self) -> i64 {
    self.lap_ms.saturating_sub(self.started_ms)
  }
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
  fn the_host_is_woken_when_the_focus_is_caught_up() {
    use futures_util::FutureExt;
    let focus = CatchUpFocus::default();
    assert_eq!(focus.caught_up_since(0).now_or_never(), None);
    focus.clone().note_caught_up();
    assert_eq!(focus.caught_up_since(0).now_or_never(), Some(1));
    assert_eq!(focus.caught_up_since(1).now_or_never(), None);
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
