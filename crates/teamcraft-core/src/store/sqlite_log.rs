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

/// What the write-ahead log still owes a purge.
pub(crate) struct PurgeState {
  /// Something was written since the log was last truncated, and a purge was asked for.
  pub(crate) is_due: bool,
  /// The caller wants it now (key deletion), not when the interval has passed.
  pub(crate) is_forced: bool,
  maybe_last_attempt: Option<Instant>,
  min_interval: Duration,
}

impl PurgeState {
  pub(crate) fn new() -> Self {
    Self {
      is_due: false,
      is_forced: false,
      maybe_last_attempt: None,
      min_interval: PURGE_INTERVAL,
    }
  }

  /// A purge that runs at once, for a write that must not linger (a deleted key).
  pub(crate) fn force(&mut self) {
    self.is_due = true;
    self.is_forced = true;
  }

  /// Truncates the log if a purge is due and its time has come. A checkpoint that did not complete
  /// (another connection holds the log, the disk failed) leaves the purge due; it is retried by the
  /// next write, purge or drop, never reported as a failure of the write that asked for it.
  pub(crate) fn run_if_due(&mut self, connection: &Connection) {
    let now = Instant::now();
    if !self.should_run(now) {
      return;
    }
    self.maybe_last_attempt = Some(now);
    match checkpoint_truncate(connection) {
      Ok(true) => {
        self.is_due = false;
        self.is_forced = false;
      },
      Ok(false) => tracing::warn!("the write-ahead log could not be truncated yet; trying again later"),
      Err(error) => tracing::warn!(%error, "could not truncate the write-ahead log; trying again later"),
    }
  }

  fn should_run(&self, now: Instant) -> bool {
    self.is_due
      && (self.is_forced
        || self
          .maybe_last_attempt
          .is_none_or(|last| now.saturating_duration_since(last) >= self.min_interval))
  }
}

/// `PRAGMA wal_checkpoint(TRUNCATE)`; `true` only if it ran to the end: not blocked by a reader
/// or writer (`busy`) and every frame of the log written back.
fn checkpoint_truncate(connection: &Connection) -> Result<bool, StoreError> {
  let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = connection
    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
      Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })
    .map_err(sqlite_error)?;
  // Without a log (a database that is not in WAL mode) SQLite answers -1 for both counts.
  Ok(busy == 0 && log_frames == checkpointed_frames)
}
