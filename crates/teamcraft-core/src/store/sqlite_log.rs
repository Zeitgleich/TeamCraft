//! The write-ahead log of the SQLite stores and its purge (docs/e2ee.md §4.3.1, §9.5): commits go
//! to the log, and old page versions are cleared from it by a truncating checkpoint, at once after
//! a deletion and at most every [`PURGE_INTERVAL`] after the many small saves of the MLS state.

use std::time::{Duration, Instant};

use rusqlite::Connection;

use super::StoreError;
use super::sql_group::sqlite_error;

/// Journal settings both SQLite stores open with. WAL: a commit appends to the log, so a write is
/// one sync instead of the rollback journal's create, two syncs of the journal, a sync of the
/// database and the journal's deletion (issue #972). `synchronous = NORMAL`: the log is synced at
/// checkpoints, not on every commit; a commit is still atomic and survives a crash of the app, and
/// only a power loss or OS crash may take back the last commits, which the client repairs like a
/// lost write (the sync reads what came after its cursors again, the outbox sends again with the
/// same idempotency key).
pub(crate) const LOG_PRAGMAS: &str = "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;";
/// The most often a debounced purge ([`super::LocalStore::purge_freed_soon`]) truncates the log: a
/// checkpoint costs two or three syncs, and the MLS storage is saved for every processed message.
/// Old snapshots can stay in the log this long while the app runs (docs/e2ee.md §4.3.1).
const PURGE_INTERVAL: Duration = Duration::from_secs(30);
/// How often a forced purge that could not finish (a read thread held its snapshot) is tried again.
/// Not after every group of writes: each try writes the log back and syncs, under the writer's
/// lock, so on a phone's flash every write and every read that waits for the writer paid for it
/// while a sync ran (a channel stayed empty for half a minute, 1.0.29).
const FORCED_RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// What the write-ahead log still owes a purge.
pub(crate) struct PurgeState {
  /// Something was written since the log was last truncated, and a purge was asked for.
  pub(crate) is_due: bool,
  /// The caller wants it now (key deletion), not when the interval has passed.
  pub(crate) is_forced: bool,
  /// A forced purge did not finish: it is tried again every [`FORCED_RETRY_INTERVAL`], not only
  /// every [`PURGE_INTERVAL`].
  is_retrying_forced: bool,
  maybe_last_attempt: Option<Instant>,
  min_interval: Duration,
  /// How long the connection's other statements wait for a lock: given back after a checkpoint,
  /// which itself never waits.
  busy_timeout: Duration,
}

impl PurgeState {
  /// For a connection whose statements wait `busy_timeout` for another connection's lock.
  pub(crate) fn new(busy_timeout: Duration) -> Self {
    Self {
      is_due: false,
      is_forced: false,
      is_retrying_forced: false,
      maybe_last_attempt: None,
      min_interval: PURGE_INTERVAL,
      busy_timeout,
    }
  }

  /// A purge that runs at once, for a write that must not linger (a deleted key).
  pub(crate) fn force(&mut self) {
    self.is_due = true;
    self.is_forced = true;
  }

  /// Truncates the log if a purge is due and its time has come. A checkpoint that did not complete
  /// (another connection holds the log, the disk failed) leaves the purge due; it is retried by a
  /// later write, purge or drop, never reported as a failure of the write that asked for it. A
  /// forced one is retried after [`FORCED_RETRY_INTERVAL`], not by the very next group of writes.
  pub(crate) fn run_if_due(&mut self, connection: &Connection) {
    let now = Instant::now();
    if !self.should_run(now) {
      return;
    }
    self.maybe_last_attempt = Some(now);
    let is_done = match checkpoint_truncate(connection, self.busy_timeout) {
      Ok(true) => true,
      // A read thread holds a snapshot: expected, and retried later.
      Ok(false) => {
        tracing::debug!("the write-ahead log could not be truncated yet; trying again later");
        false
      },
      Err(error) => {
        tracing::warn!(%error, "could not truncate the write-ahead log; trying again later");
        false
      },
    };
    self.note_attempt(is_done);
  }

  fn note_attempt(&mut self, is_done: bool) {
    if is_done {
      self.is_due = false;
      self.is_forced = false;
      self.is_retrying_forced = false;
      return;
    }
    if std::mem::take(&mut self.is_forced) {
      self.is_retrying_forced = true;
    }
  }

  fn should_run(&self, now: Instant) -> bool {
    let interval = if self.is_retrying_forced {
      FORCED_RETRY_INTERVAL
    } else {
      self.min_interval
    };
    self.is_due
      && (self.is_forced
        || self
          .maybe_last_attempt
          .is_none_or(|last| now.saturating_duration_since(last) >= interval))
  }
}

/// `PRAGMA wal_checkpoint(TRUNCATE)`; `true` only if it ran to the end: not blocked by a reader
/// or writer (`busy`) and every frame of the log written back.
///
/// It doesn't wait for a lock: a truncating checkpoint waits until every reader left its snapshot,
/// and it runs at the end of a group of writes, under the writer's locks. With the connection's
/// busy timeout it held up the engine and every read thread's write for that long whenever a read
/// thread was reading (TEAMCRAFT-F). Frames it could write back are written back; the truncation
/// is tried again later ([`PurgeState::run_if_due`]).
fn checkpoint_truncate(connection: &Connection, busy_timeout: Duration) -> Result<bool, StoreError> {
  connection.busy_timeout(Duration::ZERO).map_err(sqlite_error)?;
  let checkpointed = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
  });
  if let Err(error) = connection.busy_timeout(busy_timeout) {
    // The connection's statements then fail at once on a lock instead of waiting for it.
    tracing::warn!(%error, "could not restore the local store's busy timeout");
  }
  let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = checkpointed.map_err(sqlite_error)?;
  // Without a log (a database that is not in WAL mode) SQLite answers -1 for both counts.
  Ok(busy == 0 && log_frames == checkpointed_frames)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_forced_purge_that_did_not_finish_waits_before_the_next_try() {
    let mut purge = PurgeState::new(Duration::from_secs(1));
    purge.force();
    let tried = Instant::now();
    assert!(purge.should_run(tried), "a forced purge runs at once");
    purge.maybe_last_attempt = Some(tried);
    purge.note_attempt(false);

    assert!(purge.is_due, "it stays due");
    assert!(
      !purge.should_run(tried + Duration::from_millis(100)),
      "the next group of writes doesn't try again"
    );
    assert!(
      purge.should_run(tried + FORCED_RETRY_INTERVAL),
      "it is tried again well before the debounce interval"
    );
  }

  #[test]
  fn a_new_force_runs_at_once_even_while_retrying() {
    let mut purge = PurgeState::new(Duration::from_secs(1));
    purge.force();
    let tried = Instant::now();
    purge.maybe_last_attempt = Some(tried);
    purge.note_attempt(false);

    purge.force();
    assert!(
      purge.should_run(tried),
      "an explicit purge (closing, a deleted key) doesn't wait"
    );
  }

  #[test]
  fn a_finished_purge_goes_back_to_the_debounce_interval() {
    let mut purge = PurgeState::new(Duration::from_secs(1));
    purge.force();
    let tried = Instant::now();
    purge.maybe_last_attempt = Some(tried);
    purge.note_attempt(false);
    purge.maybe_last_attempt = Some(tried + FORCED_RETRY_INTERVAL);
    purge.note_attempt(true);

    purge.is_due = true;
    assert!(!purge.should_run(tried + FORCED_RETRY_INTERVAL * 2));
    assert!(purge.should_run(tried + FORCED_RETRY_INTERVAL + PURGE_INTERVAL));
  }
}
