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
//! writes (#1681), the user map (#1682), the signed-in start (#1758).
#![cfg(feature = "sealed-sqlite")]
#![allow(clippy::unwrap_used, reason = "test helpers: a failure fails the test")]

mod catch_up;
mod purge;
mod start;
mod vfs;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
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
  key: SymmetricKey,
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
    let key = SymmetricKey::generate(&mut rand_core::OsRng);
    Self::open_in(disk, directory, key)
  }

  /// The same store opened again, as a new process would: connections of their own, with empty
  /// caches.
  pub fn reopen(mut self) -> Self {
    let disk = self.disk.clone();
    let directory = std::mem::take(&mut self.directory);
    let key = self.key.duplicate();
    // Closed before the new connections open, as by a process that ended.
    drop(self);
    Self::open_in(disk, directory, key)
  }

  fn open_in(disk: SlowDisk, directory: PathBuf, key: SymmetricKey) -> Self {
    let path = disk.uri(&directory.join("store.db"));
    let key_source = StaticKeySource::new(key.duplicate());
    let journal = CommitJournal::new();
    let writer = Arc::new(SealedSqliteStore::open(&path, &key_source).unwrap());
    writer.publish_commits(Arc::clone(&journal));
    let open_reader = || {
      Arc::new(
        SealedSqliteStore::open_reader(
          &path,
          &key_source,
          SealedStoreLimits::default(),
          Arc::clone(&journal),
        )
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
      key,
    }
  }

  /// The write-ahead log's file, which a finished purge truncates.
  pub fn log_path(&self) -> PathBuf {
    self.directory.join("store.db-wal")
  }
}

impl Drop for SlowStore {
  fn drop(&mut self) {
    if self.directory.as_os_str().is_empty() {
      // Reopened: the files stay for the new connections.
      return;
    }
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

/// A group of writes the store logged as long (`long write group`, issue #1681).
#[derive(Clone, Debug, Default)]
pub struct LongGroup {
  /// Where the group was begun, `file:line`.
  pub site: String,
  pub held_ms: u64,
  pub writing_ms: u64,
}

/// Collects the store's `long write group` events of the thread it is set on.
#[derive(Clone, Default)]
pub struct LongGroups {
  seen: Arc<Mutex<Vec<LongGroup>>>,
}

impl LongGroups {
  /// The groups logged so far, the longest first.
  pub fn longest_first(&self) -> Vec<LongGroup> {
    let mut seen = self.seen.lock().unwrap().clone();
    seen.sort_by_key(|group| std::cmp::Reverse(group.held_ms));
    seen
  }
}

impl tracing::Subscriber for LongGroups {
  fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
    metadata.is_event()
  }

  fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
    tracing::span::Id::from_u64(1)
  }

  fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

  fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

  fn event(&self, event: &tracing::Event<'_>) {
    let mut visitor = LongGroupVisitor::default();
    event.record(&mut visitor);
    if visitor.is_long_group {
      self.seen.lock().unwrap().push(visitor.group);
    }
  }

  fn enter(&self, _: &tracing::span::Id) {}

  fn exit(&self, _: &tracing::span::Id) {}
}

#[derive(Default)]
struct LongGroupVisitor {
  is_long_group: bool,
  group: LongGroup,
}

impl tracing::field::Visit for LongGroupVisitor {
  fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
    match field.name() {
      "held_ms" => self.group.held_ms = value,
      "writing_ms" => self.group.writing_ms = value,
      _ => {},
    }
  }

  fn record_u128(&mut self, field: &tracing::field::Field, value: u128) {
    self.record_u64(field, u64::try_from(value).unwrap_or(u64::MAX));
  }

  fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
    match field.name() {
      "message" => self.is_long_group = format!("{value:?}") == "long write group",
      "site" => self.group.site = format!("{value:?}"),
      _ => {},
    }
  }
}
