//! The sealed SQLite store on a disk whose syncs are slow, as on a phone (issue #1680). On a
//! laptop a sync takes microseconds and the store never stalls; on a phone's flash it takes tens
//! to hundreds of milliseconds, and whatever syncs under the writer's lock holds up every read and
//! write that waits for it (1.0.29: reads stalled in steps of a second, #1678).
//!
//! [`SlowStore`] opens the store as the app does (a writing connection for the engine, a reading
//! connection for each of two read threads, sharing the commit journal) on a [`SlowDisk`]: a VFS whose every
//! sync waits a set delay and is counted. Scenarios assert bounds on how long a turn of the writer
//! takes (every read thread's write and every read that needs the writer waits for one) and on how
//! many syncs a group of writes costs. Bounds are generous multiples of the delay; nothing sleeps
//! for timing.
//!
//! Follow-ups add their scenarios here: a purge on a connection of its own (#1684), short groups of
//! writes (#1681), the user map (#1682).
#![cfg(feature = "sealed-sqlite")]
#![allow(clippy::unwrap_used, reason = "test helpers: a failure fails the test")]

mod purge;
mod vfs;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_core::store::{
  CommitJournal, LaneStore, SealedSqliteStore, SealedStoreLimits, StaticKeySource, StoreError,
};
use teamcraft_crypto::secret::SymmetricKey;

pub use vfs::{DiskTally, SlowDisk};

/// A sync on a slow phone: #1678 measured the stalls with this delay.
pub const PHONE_SYNC_DELAY: Duration = Duration::from_millis(50);

/// The store as the app opens it, on a slow disk.
pub struct SlowStore {
  pub disk: SlowDisk,
  /// The engine's connection: every write goes through it.
  pub writer: Arc<SealedSqliteStore>,
  /// A read thread's view (the main read lane): reads through a connection of its own, writes
  /// through `writer`.
  pub lane: LaneStore,
  /// The other read thread's connection (the query lane: searches, long walks).
  pub second_reader: Arc<SealedSqliteStore>,
  directory: PathBuf,
}

impl SlowStore {
  pub fn open(sync_delay: Duration) -> Self {
    let disk = SlowDisk::install(sync_delay);
    let directory = std::env::temp_dir().join(format!(
      "teamcraft-slow-disk-{}-{}",
      std::process::id(),
      rand_core::RngCore::next_u64(&mut rand_core::OsRng)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = disk.uri(&directory.join("store.db"));
    let key = StaticKeySource::new(SymmetricKey::generate(&mut rand_core::OsRng));
    let journal = CommitJournal::new();
    let writer = Arc::new(SealedSqliteStore::open(&path, &key).unwrap());
    writer.publish_commits(Arc::clone(&journal));
    let open_reader = || {
      Arc::new(
        SealedSqliteStore::open_reader(&path, &key, SealedStoreLimits::default(), Arc::clone(&journal))
          .unwrap(),
      )
    };
    let lane = LaneStore::reader(Arc::clone(&writer), open_reader());
    let second_reader = open_reader();
    Self {
      disk,
      writer,
      lane,
      second_reader,
      directory,
    }
  }

  /// The write-ahead log's file, which a finished purge truncates.
  pub fn log_path(&self) -> PathBuf {
    self.directory.join("store.db-wal")
  }
}

impl Drop for SlowStore {
  fn drop(&mut self) {
    // Deliberately ignored: a leftover directory in the temp dir fails nothing.
    let _ = std::fs::remove_dir_all(&self.directory);
  }
}

/// How long one turn of the writer took, and what it synced.
#[derive(Debug)]
pub struct Turn {
  pub took: Duration,
  pub syncs: DiskTally,
}

/// Runs `turn` and measures it on `disk`.
pub fn measure(disk: &SlowDisk, turn: impl FnOnce() -> Result<(), StoreError>) -> Turn {
  let before = disk.tally();
  let started = Instant::now();
  turn().unwrap();
  Turn {
    took: started.elapsed(),
    syncs: disk.tally().since(before),
  }
}
