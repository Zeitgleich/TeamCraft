//! Holes in the history held, and reading them full (#1156). A jump to a date reads the page around
//! it, which can lie well before the oldest message this device holds: between that page and what is
//! held lies history nobody read. The feed shows the rows next to each other, so the core notes where
//! each hole is and reads it, one page per request of the UI, from the side the reader comes from
//! ([`crate::ui_backend::CoreCommand::FillHistoryGap`]).
//!
//! One record per conversation in [`Table::SyncState`] holds the holes and how the last request
//! ended, like the reading above the oldest row (`older_history.rs`), whose failure rules it shares:
//! a failure stands until the next connection.
//!
//! A catch-up leaves a hole too (issue #1423): in a conversation with more new messages than
//! [`super::ClientSettings::catch_up_message_pages`] it reads the newest page ahead of the rest
//! ([`Client::note_backlog`]). That hole, its *backlog*, isn't left to the reader: the same catch-up
//! reads it full once every conversation shows its newest messages ([`Client::read_backlog`]), and
//! one cut short does in the next. A host that reads backlogs itself
//! ([`Client::read_backlogs_in_background`], issue #1448; every app's loop) does so after the sync
//! instead, a page per turn between its other work, so the end of the sync doesn't wait for them;
//! what an earlier run left is read on the next start, also on a resumed session. Messages carry
//! state (pins, bookmarks, channel role entries, edits), so none may stay unread; those whose
//! order matters are taken again in their order once the hole is read (`messaging.rs`,
//! `replay_controls_from`).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
#[cfg(feature = "ui")]
use teamcraft_client_api::model::GapEdge;
use teamcraft_proto::{ConversationId, ListMessagesQuery, MessageId, MessageResponse};

use super::Client;
#[cfg(feature = "ui")]
use super::older_history::read_connections;
use super::sync::ResyncPart;
#[cfg(feature = "ui")]
use crate::api::ApiError;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
#[cfg(feature = "ui")]
use crate::store::MessageFilter;
use crate::store::{LocalStore, MessageEdge, Table};

/// History between two messages this device never read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Gap {
  /// The newest message read before it.
  pub(crate) after: MessageId,
  /// The oldest message held after it; `None` when nothing was held after it when it was noted.
  pub(crate) maybe_before: Option<MessageId>,
}

/// The holes of a conversation and how the last request to fill one ended, under [`record_key`].
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GapsRecord {
  pub(crate) gaps: Vec<Gap>,
  /// The request answered last, also when it failed.
  pub(crate) answered: u64,
  /// The connection during which that request failed; `None` when it didn't.
  pub(crate) maybe_failed_in: Option<u64>,
  /// The hole a catch-up left and still has to read; absent from records of older versions.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub(crate) maybe_backlog: Option<Backlog>,
}

/// The new messages a catch-up left between those it read in order and the newest page, which it
/// read ahead of them (issue #1423).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Backlog {
  /// Where it starts: every hole after this message is part of it. The holes a jump to a date
  /// leaves lie before what was held, so before this.
  pub(crate) after: MessageId,
  /// The oldest message of the newest page read ahead: what was taken before the hole, and whose
  /// order-sensitive controls are taken again once the hole is read.
  pub(crate) replay_from: MessageId,
}

impl GapsRecord {
  /// The oldest hole of the backlog, if one is left.
  fn backlog_gap(&self) -> Option<Gap> {
    let backlog = self.maybe_backlog?;
    self.gaps.iter().copied().find(|gap| gap.after >= backlog.after)
  }
}

/// Where what a page read starts.
// Reading back from the newer side is the UI's (feature `ui`).
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Low {
  /// Nothing before it: the beginning was reached.
  Beginning,
  /// Every message after this ID (asked with `after`).
  After(MessageId),
  /// Every message from this one on.
  From(MessageId),
}

/// Where what a page read ends.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum High {
  /// Up to the newest message.
  Newest,
  /// Up to this message, itself included.
  Through(MessageId),
  /// Up to this message, itself not included (asked with `before`).
  Before(MessageId),
}

/// The oldest and the newest message of a page; `None` for an empty page.
pub(super) type PageBounds = Option<(MessageId, MessageId)>;

pub(super) fn bounds(page: &[MessageResponse]) -> PageBounds {
  let first = page.iter().map(|message| message.id).min()?;
  let last = page.iter().map(|message| message.id).max()?;
  Some((first, last))
}

/// What one page told for certain: every message in it, none missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Read {
  low: Low,
  maybe_first: Option<MessageId>,
  high: High,
}

impl Read {
  /// A page asked with `after`: oldest first, up to the newest when there is no more.
  fn after(cursor: MessageId, page: PageBounds, has_more: bool) -> Self {
    Self {
      low: Low::After(cursor),
      maybe_first: page.map(|(first, _)| first),
      high: match page {
        Some((_, last)) if has_more => High::Through(last),
        _ => High::Newest,
      },
    }
  }

  /// A page asked with `before`: newest first, down to the beginning when there is no more.
  #[cfg_attr(not(feature = "ui"), allow(dead_code))]
  fn before(cursor: MessageId, page: PageBounds, has_more: bool) -> Self {
    let maybe_first = page.map(|(first, _)| first);
    Self {
      low: match maybe_first {
        Some(first) if has_more => Low::From(first),
        _ => Low::Beginning,
      },
      maybe_first,
      high: High::Before(cursor),
    }
  }

  /// What is left of `gap` once this was read: nothing, itself, or one or two smaller holes.
  fn left_of(self, gap: Gap) -> Vec<Gap> {
    let is_below_gap = match self.high {
      High::Newest => false,
      High::Through(last) | High::Before(last) => last <= gap.after,
    };
    let is_above_gap = gap.maybe_before.is_some_and(|before| match self.low {
      Low::Beginning => false,
      Low::After(cursor) => cursor >= before,
      Low::From(first) => first >= before,
    });
    if is_below_gap || is_above_gap {
      return vec![gap];
    }
    let mut left = Vec::new();
    let has_lower = match self.low {
      Low::Beginning => false,
      Low::After(cursor) => gap.after < cursor,
      Low::From(first) => gap.after < first,
    };
    if has_lower {
      let maybe_before = match (gap.maybe_before, self.maybe_first) {
        (Some(before), Some(first)) => Some(before.min(first)),
        (maybe_before, maybe_first) => maybe_before.or(maybe_first),
      };
      left.push(Gap {
        after: gap.after,
        maybe_before,
      });
    }
    let maybe_upper_after = match self.high {
      High::Newest => None,
      High::Through(last) | High::Before(last) => Some(last),
    };
    if let Some(last) = maybe_upper_after
      && gap.maybe_before.is_none_or(|before| last < before)
    {
      left.push(Gap {
        after: last.max(gap.after),
        maybe_before: gap.maybe_before,
      });
    }
    left
  }
}

/// The holes once `read` was read.
fn subtract(gaps: &[Gap], read: Read) -> Vec<Gap> {
  let mut left: Vec<Gap> = gaps.iter().flat_map(|gap| read.left_of(*gap)).collect();
  left.sort_by_key(|gap| gap.after);
  left.dedup();
  left
}

/// The holes after a jump read `read`, when the oldest message held before it was `maybe_oldest`.
/// What lies before the oldest message held is not a hole but the history above it
/// (`older_history.rs`); what the page leaves between itself and that message is.
fn after_jump(gaps: &[Gap], read: Read, maybe_oldest: Option<MessageId>) -> Vec<Gap> {
  let mut left = subtract(gaps, read);
  let is_below_held = match (read.high, maybe_oldest) {
    (High::Through(last), Some(oldest)) => last < oldest,
    (High::Through(_), None) => true,
    _ => false,
  };
  if let High::Through(last) = read.high
    && is_below_held
  {
    left.push(Gap {
      after: last,
      maybe_before: maybe_oldest,
    });
    left.sort_by_key(|gap| gap.after);
    left.dedup();
  }
  left
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Notes the hole a jump's page left: `maybe_oldest` is the oldest message held before the page
  /// was stored.
  pub(super) fn note_jump_page(
    &self,
    conversation: ConversationId,
    cursor: MessageId,
    page: PageBounds,
    has_more: bool,
    maybe_oldest: Option<MessageId>,
  ) -> Result<(), CoreError> {
    let mut record = read_record(&self.store, conversation)?.unwrap_or_default();
    record.gaps = after_jump(&record.gaps, Read::after(cursor, page, has_more), maybe_oldest);
    write_record(&self.store, conversation, &record)
  }

  /// The oldest server message held, for [`Self::note_jump_page`].
  pub(super) fn oldest_held(&self, conversation: ConversationId) -> Result<Option<MessageId>, CoreError> {
    Ok(self.store.edge_server_id(conversation, MessageEdge::Oldest)?)
  }

  /// A conversation that is gone takes its holes with it.
  pub(super) fn forget_history_gaps(&self, conversation: ConversationId) -> Result<(), CoreError> {
    self.store.delete(Table::SyncState, &record_key(conversation))?;
    Ok(())
  }

  /// Notes the hole a catch-up leaves when it reads the newest page, whose oldest message is
  /// `before`, ahead of the messages after `after`. Noted before the page is stored: a catch-up cut
  /// short in between reads it again, and the hole too.
  pub(super) fn note_backlog(
    &self,
    conversation: ConversationId,
    after: MessageId,
    before: MessageId,
  ) -> Result<(), CoreError> {
    let mut record = read_record(&self.store, conversation)?.unwrap_or_default();
    record.gaps.push(Gap {
      after,
      maybe_before: Some(before),
    });
    record.gaps.sort_by_key(|gap| gap.after);
    record.gaps.dedup();
    record.maybe_backlog = Some(match record.maybe_backlog {
      // One left by a catch-up cut short: both are read.
      Some(earlier) => Backlog {
        after: earlier.after.min(after),
        replay_from: earlier.replay_from.min(before),
      },
      None => Backlog {
        after,
        replay_from: before,
      },
    });
    tracing::info!(conversation_id = %conversation, "catch-up: the newest page ahead of the rest");
    write_record(&self.store, conversation, &record)
  }

  /// Whether a catch-up left new messages of `conversation` to read.
  pub(super) fn has_backlog(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(read_record(&self.store, conversation)?.is_some_and(|record| record.maybe_backlog.is_some()))
  }

  /// Reads the backlog of `conversation` full, oldest first, one page per request, and the live
  /// events that may go ahead between two of them (`live_catch_up.rs`); then takes the controls
  /// that came before it again, in their order. Each page is noted as it is stored, so a read cut
  /// short goes on from there. Nothing to do without a backlog.
  pub(super) async fn read_backlog(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    while self.read_backlog_page(conversation).await? == BacklogStep::More {
      self.apply_live_during_catch_up().await?;
    }
    Ok(())
  }

  /// One step of [`Self::read_backlog`]: the next page of the hole, or, once it is read full, the
  /// controls that came before it taken again in their order (the end of the backlog).
  pub(super) async fn read_backlog_page(
    &mut self,
    conversation: ConversationId,
  ) -> Result<BacklogStep, CoreError> {
    let _step = crate::timing::step("backlog_read_page");
    let Some(mut record) = read_record(&self.store, conversation)? else {
      return Ok(BacklogStep::Done);
    };
    let Some(backlog) = record.maybe_backlog else {
      return Ok(BacklogStep::Done);
    };
    let Some(gap) = record.backlog_gap() else {
      // Read full (also from the reader's side, `fill_history_gap`).
      self.replay_controls_from(conversation, backlog.replay_from)?;
      record.maybe_backlog = None;
      tracing::info!(conversation_id = %conversation, "catch-up: backlog read");
      write_record(&self.store, conversation, &record)?;
      return Ok(BacklogStep::Done);
    };
    let query = ListMessagesQuery {
      maybe_before: None,
      maybe_after: Some(gap.after),
      maybe_limit: Some(self.settings.history_page_size),
    };
    let page = self.api.list_messages(conversation, &query).await?;
    let read = Read::after(gap.after, bounds(&page.messages), page.has_more);
    self.receive_page(page.messages).await?;
    record.gaps = subtract(&record.gaps, read);
    if record.backlog_gap() == Some(gap) {
      // A page that doesn't get past the hole's start (only a server ignoring `after` sends one):
      // the hole is left to the reader, rather than asked for again and again.
      tracing::warn!(conversation_id = %conversation, "catch-up: a page of the backlog read nothing new");
      record.maybe_backlog = None;
    }
    write_record(&self.store, conversation, &record)?;
    // A page fills a hole only the conversation on screen shows; elsewhere the UI hears of the
    // backlog once it is read full. A wake per page had the UI read everything again for every page
    // of a long absence, which kept the browser's only thread busy for minutes (issue #1572).
    if self.catch_up_focus.get() == Some(conversation) {
      self.note_sync_progress();
    }
    Ok(BacklogStep::More)
  }

  /// The host reads the backlogs a catch-up leaves itself, a page per turn of its loop
  /// ([`Self::is_background_history_due`], [`Self::load_background_history_page`], issue #1448), so
  /// the end of the full sync (the outbox, membership changes waiting, key packages, account data)
  /// and the session it keeps no longer wait for them. Without this the catch-up reads them before
  /// it ends, as before. Called by the host that polls, once per client; the backlogs an earlier
  /// run left are read too, also on a resumed session, where no catch-up runs.
  pub fn read_backlogs_in_background(&mut self) -> Result<(), CoreError> {
    self.background_backlogs.is_on = true;
    for (key, record) in
      records::list_json_with_prefix::<GapsRecord>(&self.store, Table::SyncState, RECORD_PREFIX)?
    {
      if record.maybe_backlog.is_some()
        && let Some(Ok(conversation)) = key.strip_prefix(RECORD_PREFIX).map(str::parse)
      {
        self.background_backlogs.due.insert(conversation);
      }
    }
    Ok(())
  }

  /// Whether a host should ask for the next page read in the background now: a backlog a catch-up
  /// left ([`Self::read_backlogs_in_background`]) or a first sync's older history.
  pub fn is_background_history_due(&self) -> bool {
    self.is_backlog_due() || self.is_older_history_due()
  }

  /// Reads one page in the background, for a host's loop while [`Self::is_background_history_due`]:
  /// of a backlog first (new messages, the focused conversation's first), else of a first sync's
  /// older history. A failure waits for its retry time. Returns whether the UI should read again.
  pub async fn load_background_history_page(&mut self) -> bool {
    if !self.is_backlog_due() {
      return Box::pin(self.load_older_history_page()).await;
    }
    let before = self.sync_progress.count();
    // Boxed: its future holds a page's requests and decryption, and sits in the host's loop.
    match Box::pin(self.read_backlog_in_background()).await {
      Ok(()) => self.background_backlogs.maybe_retry_at_ms = None,
      Err(error) => {
        tracing::debug!(%error, "backlog not read further yet");
        self.background_backlogs.maybe_retry_at_ms = Some(self.now_ms().saturating_add(BACKLOG_RETRY_MS));
      },
    }
    self.sync_progress.count() != before
  }

  /// A connection came up: a backlog page that failed is tried again at once.
  pub(super) fn note_backlog_connection(&mut self) {
    self.background_backlogs.maybe_retry_at_ms = None;
  }

  /// Whether a backlog waits for the host's loop and its retry time, if any, has come: a host that
  /// waits for the catch-up waits for this too (the runner's `RunnerCommand::CatchUp`).
  pub fn is_backlog_due(&self) -> bool {
    let backlogs = &self.background_backlogs;
    if backlogs.due.is_empty() {
      return false;
    }
    let Some(retry_at_ms) = backlogs.maybe_retry_at_ms else {
      return true;
    };
    let now = self.now_ms();
    // Further ahead than a wait can be: the clock went back.
    now >= retry_at_ms || retry_at_ms.saturating_sub(now) > BACKLOG_RETRY_MS
  }

  /// The end of a catch-up whose host reads the backlogs: notes those it leaves.
  pub(super) fn leave_backlogs_to_host(&mut self, conversations: &[ConversationId]) -> Result<(), CoreError> {
    for conversation in conversations {
      if self.has_backlog(*conversation)? {
        self.background_backlogs.due.insert(*conversation);
      }
    }
    if !self.background_backlogs.due.is_empty() {
      tracing::info!(
        backlogs = self.background_backlogs.due.len(),
        "catch-up: backlogs left to read in the background"
      );
    }
    Ok(())
  }

  /// One page of the focused conversation's backlog, or else of the first due; at its end what its
  /// messages need, as in the catch-up (`catch_up_backlogs`). A failure other than the network's
  /// marks it for another try (`resync_marked_conversations`) and lets the others go on.
  async fn read_backlog_in_background(&mut self) -> Result<(), CoreError> {
    let backlogs = &self.background_backlogs.due;
    let Some(conversation) = self
      .catch_up_focus
      .get()
      .filter(|focus| backlogs.contains(focus))
      .or_else(|| backlogs.first().copied())
    else {
      return Ok(());
    };
    // Read meanwhile from the reader's side, by a resync, or the conversation went.
    let result = if self.has_backlog(conversation)? {
      match self.read_backlog_page(conversation).await {
        Ok(BacklogStep::More) => return Ok(()),
        Ok(BacklogStep::Done) => self.finish_backlog(conversation).await,
        Err(error) => Err(error),
      }
    } else {
      Ok(())
    };
    // A network failure propagates: the backlog stays due and is tried again.
    self.isolate(conversation, ResyncPart::Content, result)?;
    self.background_backlogs.due.remove(&conversation);
    self
      .store
      .delete(Table::SyncState, &super::catch_up::marker_key(conversation))?;
    self.note_sync_progress();
    Ok(())
  }

  /// What a backlog's messages need once it is read: those waiting for keys, and their senders'
  /// keys.
  pub(super) async fn finish_backlog(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.retry_undecrypted(conversation)?;
    self.learn_sender_keys(conversation).await
  }
}

/// How long a backlog page that failed waits for its next try in the background (offline, server
/// trouble); a new connection tries again at once.
const BACKLOG_RETRY_MS: i64 = 30_000;

/// What [`Client::read_backlog_page`] left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BacklogStep {
  /// More of the hole to read.
  More,
  /// Read full, or nothing was left.
  Done,
}

/// The backlogs a catch-up leaves to the host's loop (issue #1448), beside their records.
#[derive(Debug, Default)]
pub(super) struct BackgroundBacklogs {
  /// The host reads them ([`Client::read_backlogs_in_background`]); otherwise the catch-up does.
  pub(super) is_on: bool,
  /// Conversations with a backlog still to read.
  due: BTreeSet<ConversationId>,
  /// After a failure: not before this time.
  maybe_retry_at_ms: Option<i64>,
}

#[cfg(feature = "ui")]
impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Answers request `request` of the UI with one page of the hole after `after`, from its `edge`.
  /// A failure is the answer, shown where the hole is; only a refused token comes back as an error,
  /// for the host that refreshes it and runs the command again.
  pub async fn fill_history_gap(
    &mut self,
    conversation: ConversationId,
    after: MessageId,
    edge: GapEdge,
    request: u64,
  ) -> Result<(), CoreError> {
    let _step = crate::timing::step("history_gap_fill");
    let mut record = read_record(&self.store, conversation)?.unwrap_or_default();
    let maybe_gap = record.gaps.iter().copied().find(|gap| gap.after == after);
    let outcome = match maybe_gap {
      Some(gap) => self.read_gap(conversation, gap, edge).await.map(Some),
      // Filled or gone since the UI asked: nothing to read.
      None => Ok(None),
    };
    record.answered = request;
    record.maybe_failed_in = None;
    match &outcome {
      Ok(Some((gap, read))) => {
        // A hole whose end was unknown ends at the first message held after it.
        for held in &mut record.gaps {
          if held.after == gap.after {
            *held = *gap;
          }
        }
        record.gaps = subtract(&record.gaps, *read);
      },
      Ok(None) => {},
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not read a hole in the history");
        record.maybe_failed_in = Some(read_connections(&self.store)?);
      },
    }
    write_record(&self.store, conversation, &record)?;
    match outcome {
      Err(error @ CoreError::Api(ApiError::Status { status: 401, .. })) => Err(error),
      _ => Ok(()),
    }
  }

  /// Reads and stores one page of `gap`; returns the hole with its end as known now, and what the
  /// page read.
  async fn read_gap(
    &mut self,
    conversation: ConversationId,
    mut gap: Gap,
    edge: GapEdge,
  ) -> Result<(Gap, Read), CoreError> {
    if gap.maybe_before.is_none() {
      gap.maybe_before = self.first_held_after(conversation, gap.after)?;
    }
    let maybe_newer_side = gap.maybe_before.filter(|_| edge == GapEdge::Newer);
    let query = ListMessagesQuery {
      maybe_before: maybe_newer_side,
      // Nothing held after it to read back from: from the older side.
      maybe_after: maybe_newer_side.is_none().then_some(gap.after),
      maybe_limit: Some(self.settings.history_page_size),
    };
    let page = self.api.list_messages(conversation, &query).await?;
    let read = match maybe_newer_side {
      Some(before) => Read::before(before, bounds(&page.messages), page.has_more),
      None => Read::after(gap.after, bounds(&page.messages), page.has_more),
    };
    self.receive_earlier_page(conversation, page.messages).await?;
    Ok((gap, read))
  }

  /// The oldest server message held after `after` (a hole noted while nothing was held after it).
  /// Reads the conversation whole; only such a hole asks.
  fn first_held_after(
    &self,
    conversation: ConversationId,
    after: MessageId,
  ) -> Result<Option<MessageId>, CoreError> {
    let mut maybe_first: Option<MessageId> = None;
    self
      .store
      .visit_messages(conversation, MessageFilter::All, &mut |message| {
        if message.maybe_unreadable.is_some() {
          return;
        }
        if let Some(id) = message.maybe_server_id
          && id > after
        {
          maybe_first = Some(maybe_first.map_or(id, |first| first.min(id)));
        }
      })?;
    Ok(maybe_first)
  }
}

/// The record of `conversation`.
#[cfg_attr(not(feature = "ui"), allow(dead_code))]
pub(crate) fn read_record(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Option<GapsRecord>, CoreError> {
  records::get_json(store, Table::SyncState, &record_key(conversation))
}

fn write_record(
  store: &impl LocalStore,
  conversation: ConversationId,
  record: &GapsRecord,
) -> Result<(), CoreError> {
  let key = record_key(conversation);
  if record.gaps.is_empty() && record.answered == 0 && record.maybe_backlog.is_none() {
    store.delete(Table::SyncState, &key)?;
    return Ok(());
  }
  records::put_json(store, Table::SyncState, &key, record)
}

fn record_key(conversation: ConversationId) -> String {
  format!("{RECORD_PREFIX}{conversation}")
}

/// Key prefix of the records in [`Table::SyncState`], one per conversation with holes.
const RECORD_PREFIX: &str = "history_gaps/";

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_proto::{Id, Snowflake};

  fn id(raw: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(raw).expect("positive"))
  }

  fn gap(after: i64, maybe_before: Option<i64>) -> Gap {
    Gap {
      after: id(after),
      maybe_before: maybe_before.map(id),
    }
  }

  fn after(cursor: i64, first: i64, last: Option<i64>) -> Read {
    Read {
      low: Low::After(id(cursor)),
      maybe_first: Some(id(first)),
      high: last.map_or(High::Newest, |last| High::Through(id(last))),
    }
  }

  #[test]
  fn a_page_before_what_is_held_leaves_a_hole_up_to_it() {
    let gaps = after_jump(&[], after(5, 10, Some(20)), Some(id(100)));
    assert_eq!(gaps, vec![gap(20, Some(100))]);
  }

  #[test]
  fn a_page_that_reaches_what_is_held_leaves_none() {
    assert!(after_jump(&[], after(5, 10, Some(120)), Some(id(100))).is_empty());
    assert!(
      after_jump(&[], after(5, 10, None), Some(id(100))).is_empty(),
      "a page up to the newest"
    );
  }

  #[test]
  fn a_page_inside_held_history_leaves_none() {
    assert!(after_jump(&[], after(150, 160, Some(170)), Some(id(100))).is_empty());
  }

  #[test]
  fn a_page_inside_a_hole_splits_it() {
    let gaps = after_jump(&[gap(20, Some(100))], after(40, 45, Some(50)), Some(id(10)));
    assert_eq!(gaps, vec![gap(20, Some(45)), gap(50, Some(100))]);
  }

  #[test]
  fn a_page_before_an_older_hole_adds_one() {
    let gaps = after_jump(&[gap(20, Some(100))], after(1, 2, Some(5)), Some(id(10)));
    assert_eq!(gaps, vec![gap(5, Some(10)), gap(20, Some(100))]);
  }

  #[test]
  fn reading_from_the_older_side_moves_the_hole_and_closes_it() {
    let gaps = subtract(&[gap(20, Some(100))], after(20, 21, Some(40)));
    assert_eq!(gaps, vec![gap(40, Some(100))]);
    assert!(subtract(&gaps, after(40, 41, None)).is_empty(), "no more");
    assert!(
      subtract(&gaps, after(40, 41, Some(100))).is_empty(),
      "up to the message held"
    );
  }

  #[test]
  fn reading_from_the_newer_side_moves_the_hole_and_closes_it() {
    let gaps = subtract(
      &[gap(20, Some(100))],
      Read::before(id(100), Some((id(80), id(99))), true),
    );
    assert_eq!(gaps, vec![gap(20, Some(80))]);
    assert!(
      subtract(&gaps, Read::before(id(80), Some((id(15), id(79))), true)).is_empty(),
      "past the message before it"
    );
    assert!(
      subtract(&gaps, Read::before(id(80), None, false)).is_empty(),
      "nothing between"
    );
  }

  #[test]
  fn other_holes_stay() {
    let gaps = subtract(&[gap(5, Some(10)), gap(20, Some(100))], after(20, 21, Some(40)));
    assert_eq!(gaps, vec![gap(5, Some(10)), gap(40, Some(100))]);
  }
}
