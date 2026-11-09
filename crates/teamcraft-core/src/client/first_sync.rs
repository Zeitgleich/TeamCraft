//! The first sync of a device that was just set up (enrollment, linking, recovery): the conversation
//! list shows at once, a bar under the top bar says how far the messages are, and what arrives stays
//! in the store, so later starts read only what changed (`heads.rs`).
//!
//! Two stages, both counted in conversations, the only total known up front (the server keeps no
//! message count per conversation):
//!
//! - **Newest:** the catch-up reads the newest page of every conversation (`catch_up.rs`).
//! - **Older:** one page at a time, what the device keeps ([`HistoryBackfill`]): the last three
//!   months of each conversation, the most recently active first, the one the user looks at ahead
//!   of them. A host calls [`Client::load_background_history_page`] whenever nothing else is
//!   waiting (it reads the backlogs a catch-up left first, issue #1448), so reads, sends and gateway
//!   events go first and nothing here runs while the app is in the background or offline.
//!
//! Everything lives in [`Table::SyncState`]: one small record, and per conversation in the older
//! stage the cursor it stands at, written with its page. An app killed half way goes on where it
//! stood. A device that holds history already (an update of the app) never starts this.

use std::cmp::Reverse;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, Id, ListMessagesQuery, MessageId, Snowflake};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, MessageEdge, Table};

const FIRST_SYNC_KEY: &str = "first_sync";
const OLDER_CURSOR_PREFIX: &str = "first_sync_older/";

/// How long a page that failed (offline, server trouble, rate limit) waits for its next try. A new
/// gateway connection doesn't shorten it: the catch-up after it has enough to do.
const OLDER_RETRY: Duration = Duration::from_secs(30);

/// Whether this build keeps the record. The browser doesn't: it reads the newest page only, each
/// conversation already says "catching up" there (`catch_up.rs`), and a guest's download has no room
/// for this bookkeeping (robustness.md, "Download size").
const TRACKS_FIRST_SYNC: bool = !cfg!(target_arch = "wasm32");

/// How much older history a newly set up device reads ahead. What lies before it stays on the server
/// and comes when the reader scrolls there (`older_history.rs`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryBackfill {
  /// How far back from now. The local clock only decides how much is read ahead; a wrong one reads
  /// more or less, within the limits below.
  pub window: Duration,
  /// Most messages read per conversation: a very busy channel doesn't take the budget of the others.
  pub max_per_conversation: usize,
  /// Most messages read overall, so the store stays within what docs/performance.md measures.
  pub max_messages: usize,
}

/// How far the first sync of this device is, for the bar under the top bar. Numbers from the
/// record, never an estimate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FirstSyncProgress {
  pub stage: FirstSyncStage,
  /// Conversations done in this stage.
  pub done: usize,
  /// Conversations in this stage; 0 until the list is here.
  pub total: usize,
  /// How far overall, from 0 to 1, each conversation counting once per stage; `None` while nothing
  /// finished yet (the member lists and welcomes come first), for an indeterminate bar.
  pub maybe_fraction: Option<f32>,
}

/// What the first sync is reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FirstSyncStage {
  /// The newest messages of every conversation.
  Newest,
  /// Older messages, in the background.
  Older,
}

/// What this process knows beside the record.
#[derive(Debug, Default)]
pub(super) struct FirstSyncState {
  /// The older stage is on, so a host's loop has pages to ask for without reading the store.
  is_loading_older: bool,
  /// No page before this time (after a failure).
  maybe_retry_at_ms: Option<i64>,
}

/// The record under [`FIRST_SYNC_KEY`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct FirstSyncRecord {
  stage: Stage,
  /// Conversations of the first sync; 0 until the list is here.
  total: usize,
  /// Those the stage hasn't finished, in the order the older stage reads them.
  waiting: Vec<ConversationId>,
  /// Messages read so far by the older stage, of the conversations finished.
  older_messages: usize,
  has_older_stage: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
  /// A new device, before the conversation list arrived.
  Listing,
  Newest,
  Older,
  /// Nothing more to do, also on a device that never was new to this.
  Finished,
}

/// Where the older stage stands in a conversation, under its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OlderCursor {
  before: MessageId,
  loaded: usize,
}

/// How the older stage's request for a conversation went.
enum PageOutcome {
  /// More to read.
  More,
  /// This conversation is done (its beginning or a limit reached, or read refused for good).
  Finished { loaded: usize },
}

impl HistoryBackfill {
  /// The newest page only: for a host whose store mustn't grow with history (the browser reads all
  /// of its ciphertext when it opens, docs/robustness-audit.md).
  pub const OFF: Self = Self {
    window: Duration::ZERO,
    max_per_conversation: 0,
    max_messages: 0,
  };

  /// What a phone or desktop keeps: three months, within the 100,000 messages the budgets of
  /// robustness.md are measured with.
  pub const KEEP_RECENT: Self = Self {
    window: Duration::from_secs(90 * 24 * 60 * 60),
    max_per_conversation: 20_000,
    max_messages: 100_000,
  };

  fn is_off(&self) -> bool {
    self.max_messages == 0 || self.max_per_conversation == 0 || self.window.is_zero()
  }
}

/// The first sync's progress, or `None` when it isn't running. Reads one small record.
pub fn first_sync_progress(store: &impl LocalStore) -> Result<Option<FirstSyncProgress>, CoreError> {
  if !TRACKS_FIRST_SYNC {
    return Ok(None);
  }
  let Some(record) = read_record(store)? else {
    return Ok(None);
  };
  let stage = match record.stage {
    Stage::Listing | Stage::Newest => FirstSyncStage::Newest,
    Stage::Older => FirstSyncStage::Older,
    Stage::Finished => return Ok(None),
  };
  let done = record.total.saturating_sub(record.waiting.len());
  let (before, stages) = match (stage, record.has_older_stage) {
    (FirstSyncStage::Older, _) => (record.total, 2),
    (FirstSyncStage::Newest, true) => (0, 2),
    (FirstSyncStage::Newest, false) => (0, 1),
  };
  let steps_done = before + done;
  let maybe_fraction = (steps_done > 0).then(|| ratio(steps_done, record.total * stages));
  Ok(Some(FirstSyncProgress {
    stage,
    done,
    total: record.total,
    maybe_fraction,
  }))
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Whether a host should ask for the next page of older history now.
  pub fn is_older_history_due(&self) -> bool {
    if !self.first_sync.is_loading_older {
      return false;
    }
    let Some(retry_at_ms) = self.first_sync.maybe_retry_at_ms else {
      return true;
    };
    let now = self.now_ms();
    // Further ahead than a wait can be: the clock went back.
    now >= retry_at_ms || retry_at_ms.saturating_sub(now) > millis(OLDER_RETRY)
  }

  /// Reads one page of older history, for a host's loop while [`Self::is_older_history_due`]. A
  /// failure waits for its retry time, whatever it was. Returns whether the progress the UI shows
  /// changed: a page alone doesn't, so a long read doesn't make the UI read everything again.
  pub async fn load_older_history_page(&mut self) -> bool {
    let _step = crate::timing::step("first_sync_older_page");
    let before = self.sync_progress.count();
    match self.read_older_page().await {
      Ok(()) => self.first_sync.maybe_retry_at_ms = None,
      Err(error) => {
        tracing::debug!(%error, "older history not read further yet");
        self.first_sync.maybe_retry_at_ms = Some(self.now_ms().saturating_add(millis(OLDER_RETRY)));
      },
    }
    self.sync_progress.count() != before
  }

  /// At the start of a full sync: a device with no conversation stored shows the bar from now on,
  /// before the list is here.
  pub(super) fn note_first_sync_starting(&mut self) -> Result<(), CoreError> {
    if !TRACKS_FIRST_SYNC {
      return Ok(());
    }
    if read_record(&self.store)?.is_some() || !self.store.list(Table::Conversations)?.is_empty() {
      return Ok(());
    }
    self.write_record(&FirstSyncRecord {
      stage: Stage::Listing,
      total: 0,
      waiting: Vec::new(),
      older_messages: 0,
      has_older_stage: !self.settings.history_backfill.is_off(),
    })
  }

  /// The conversation list is stored: the first sync of a new device knows its total now. A device
  /// that has read history before (an update of the app) is done with this for good.
  pub(super) fn note_first_sync_listed(&mut self, listed: &[ConversationId]) -> Result<(), CoreError> {
    if !TRACKS_FIRST_SYNC {
      return Ok(());
    }
    let maybe_record = read_record(&self.store)?;
    if maybe_record
      .as_ref()
      .is_some_and(|record| record.stage != Stage::Listing)
    {
      return Ok(());
    }
    let mut is_known_device = false;
    for conversation in listed {
      if self.is_history_loaded(*conversation)? {
        is_known_device = true;
        break;
      }
    }
    let stage = if is_known_device || listed.is_empty() {
      Stage::Finished
    } else {
      Stage::Newest
    };
    self.write_record(&FirstSyncRecord {
      stage,
      total: listed.len(),
      waiting: if stage == Stage::Newest {
        listed.to_vec()
      } else {
        Vec::new()
      },
      older_messages: 0,
      has_older_stage: !self.settings.history_backfill.is_off(),
    })
  }

  /// A catch-up starts: what it doesn't read was read before an interruption.
  pub(super) fn note_first_sync_due(&mut self, due: &[ConversationId]) -> Result<(), CoreError> {
    if !TRACKS_FIRST_SYNC {
      return Ok(());
    }
    let Some(mut record) = read_record(&self.store)?.filter(|record| record.stage == Stage::Newest) else {
      return Ok(());
    };
    let waiting_before = record.waiting.len();
    record.waiting.retain(|conversation| due.contains(conversation));
    if record.waiting.len() != waiting_before {
      self.write_record(&record)?;
    }
    Ok(())
  }

  /// The catch-up finished a conversation, whatever came of it: one it had to skip is the upkeep's
  /// to retry (`resync_marked_conversations`) and must not hold up the bar.
  pub(super) fn note_first_sync_caught_up(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if !TRACKS_FIRST_SYNC {
      return Ok(());
    }
    let Some(mut record) = read_record(&self.store)?.filter(|record| record.stage == Stage::Newest) else {
      return Ok(());
    };
    let waiting_before = record.waiting.len();
    record.waiting.retain(|waiting| *waiting != conversation);
    if record.waiting.len() != waiting_before {
      self.write_record(&record)?;
    }
    Ok(())
  }

  /// The catch-up is through: on to older history, the most recently active conversation first.
  pub(super) fn finish_first_sync_newest(&mut self, listed: &[ConversationId]) -> Result<(), CoreError> {
    if !TRACKS_FIRST_SYNC {
      return Ok(());
    }
    let Some(mut record) = read_record(&self.store)?.filter(|record| record.stage == Stage::Newest) else {
      return Ok(());
    };
    if self.settings.history_backfill.is_off() {
      return self.finish_first_sync(record);
    }
    let mut waiting = listed.to_vec();
    waiting.sort_by_key(|conversation| Reverse(self.head_message(*conversation)));
    record.stage = Stage::Older;
    record.total = waiting.len();
    record.waiting = waiting;
    self.write_record(&record)?;
    tracing::info!(conversations = record.total, "first sync: reading older history");
    Ok(())
  }

  /// One page of the conversation in turn; the focused one goes ahead of the others.
  async fn read_older_page(&mut self) -> Result<(), CoreError> {
    let Some(mut record) = read_record(&self.store)?.filter(|record| record.stage == Stage::Older) else {
      self.first_sync.is_loading_older = false;
      return Ok(());
    };
    let backfill = self.settings.history_backfill;
    let maybe_focus = self
      .catch_up_focus
      .get()
      .filter(|focus| record.waiting.contains(focus));
    let Some(conversation) = maybe_focus.or_else(|| record.waiting.first().copied()) else {
      return self.finish_first_sync(record);
    };
    if record.older_messages >= backfill.max_messages {
      tracing::info!(
        messages = record.older_messages,
        "first sync: read as much as this device keeps"
      );
      for conversation in std::mem::take(&mut record.waiting) {
        self.store.delete(Table::SyncState, &cursor_key(conversation))?;
      }
      return self.finish_first_sync(record);
    }
    if let PageOutcome::Finished { loaded } = self.read_page_of(conversation, record.older_messages).await? {
      // Senders this device has no verified key for yet; their messages wait for it till now.
      if self.parked_conversations()?.contains(&conversation) {
        self.retry_undecrypted(conversation)?;
        self.learn_sender_keys(conversation).await?;
      }
      self.store.delete(Table::SyncState, &cursor_key(conversation))?;
      record.waiting.retain(|waiting| *waiting != conversation);
      record.older_messages = record.older_messages.saturating_add(loaded);
      self.write_record(&record)?;
    }
    Ok(())
  }

  /// Reads and stores the page before where `conversation` stands, with its cursor in one group of
  /// writes: a process killed in between reads the page again, and receiving is idempotent.
  async fn read_page_of(
    &mut self,
    conversation: ConversationId,
    older_messages: usize,
  ) -> Result<PageOutcome, CoreError> {
    let key = cursor_key(conversation);
    let maybe_cursor = records::get_json::<OlderCursor>(&self.store, Table::SyncState, &key)?;
    let loaded = maybe_cursor.map_or(0, |cursor| cursor.loaded);
    let maybe_before = match maybe_cursor {
      Some(cursor) => Some(cursor.before),
      None => self.store.edge_server_id(conversation, MessageEdge::Oldest)?,
    };
    let backfill = self.settings.history_backfill;
    let window_starts = self.window_start(backfill.window);
    if maybe_before.is_some_and(|before| before <= window_starts) {
      return Ok(PageOutcome::Finished { loaded });
    }
    let budget = backfill.max_messages.saturating_sub(older_messages);
    let query = ListMessagesQuery {
      maybe_before,
      maybe_after: None,
      maybe_limit: Some(self.settings.history_page_size),
    };
    let page = match self.api.list_messages(conversation, &query).await {
      Ok(page) => page,
      Err(error) if error.is_retry_later() => return Err(error.into()),
      // Refused for good (left meanwhile, no longer allowed): nothing more to read here.
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "stopped reading older history");
        return Ok(PageOutcome::Finished { loaded });
      },
    };
    let loaded = loaded.saturating_add(page.messages.len());
    // The page's own oldest message is the next cursor, not the store's: a message parked for its
    // key isn't stored yet, and the same page would be read again.
    let maybe_oldest = page.messages.iter().map(|message| message.id).min();
    let maybe_next = maybe_oldest.filter(|oldest| {
      page.has_more && *oldest > window_starts && loaded < backfill.max_per_conversation && loaded < budget
    });
    // Read newest first: an older pin or bookmarks message mustn't win (issue #1438).
    let received = super::messaging::received_ids(conversation, &page.messages);
    self
      .receive_in_groups(page.messages, |client| {
        client.settle_ordered_controls(conversation, &received)?;
        if let Some(before) = maybe_next {
          records::put_json(
            &client.store,
            Table::SyncState,
            &key,
            &OlderCursor { before, loaded },
          )?;
        }
        Ok(())
      })
      .await?;
    Ok(match maybe_next {
      Some(_) => PageOutcome::More,
      None => PageOutcome::Finished { loaded },
    })
  }

  /// The newest ID sent before the window (IDs are snowflakes, so the time is a cursor).
  fn window_start(&self, window: Duration) -> MessageId {
    let starts_ms = self.now_ms().saturating_sub(millis(window));
    MessageId::from_snowflake(Snowflake::cursor_before(starts_ms))
  }

  fn finish_first_sync(&mut self, mut record: FirstSyncRecord) -> Result<(), CoreError> {
    tracing::info!(messages = record.older_messages, "first sync done");
    record.stage = Stage::Finished;
    record.waiting = Vec::new();
    self.write_record(&record)
  }

  fn write_record(&mut self, record: &FirstSyncRecord) -> Result<(), CoreError> {
    records::put_json(&self.store, Table::SyncState, FIRST_SYNC_KEY, record)?;
    self.first_sync.is_loading_older = record.stage == Stage::Older;
    self.note_sync_progress();
    Ok(())
  }

  /// After opening the store: older history goes on where it stood.
  pub(super) fn resume_first_sync(&mut self) -> Result<(), CoreError> {
    if !TRACKS_FIRST_SYNC {
      return Ok(());
    }
    self.first_sync.is_loading_older =
      read_record(&self.store)?.is_some_and(|record| record.stage == Stage::Older);
    Ok(())
  }
}

fn read_record(store: &impl LocalStore) -> Result<Option<FirstSyncRecord>, CoreError> {
  records::get_json(store, Table::SyncState, FIRST_SYNC_KEY)
}

fn cursor_key(conversation: ConversationId) -> String {
  format!("{OLDER_CURSOR_PREFIX}{conversation}")
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// A fraction for a bar; rounding is fine here.
#[allow(clippy::cast_precision_loss)]
fn ratio(done: usize, total: usize) -> f32 {
  if total == 0 {
    return 0.0;
  }
  (done as f32 / total as f32).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_backfill_without_a_budget_is_off() {
    assert!(HistoryBackfill::OFF.is_off());
    assert!(!HistoryBackfill::KEEP_RECENT.is_off());
  }
}
