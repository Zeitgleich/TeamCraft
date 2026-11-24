//! The store the web core runs on (issue #2363): one type for the records in IndexedDB (a core in
//! the page: guests, and members until their store moved) and on OPFS (a core in a dedicated
//! worker, e2ee.md §9.1), so the core and everything that takes its client is written once.
//!
//! Each method says which backend does what; where they differ (flushing is asynchronous on
//! IndexedDB, synchronous on OPFS), the async form serves both.

use std::ops::ControlFlow;

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;

use super::idb::OpenMode;
use super::indexed_db::{IndexedDbRecords, build_catalog_committing};
use super::opfs::{OpfsOpening, OpfsRecords, build_catalog_yielding};
use crate::store::sealed::{LazySeal, RecordBackend, SealedStore};
use crate::store::{CommitStamp, SealedStoreLimits, StoreError, WriteTally};

/// The web core's encrypted store, on either backend.
pub type BrowserStore = SealedStore<BrowserRecords>;

/// Where the web core's sealed records are.
pub enum BrowserRecords {
  /// Every record's ciphertext read at open, writes batched into transactions.
  IndexedDb(IndexedDbRecords),
  /// A record log on sync access handles, read per record; only in a dedicated worker.
  Opfs(OpfsRecords),
}

impl SealedStore<BrowserRecords> {
  /// The store `name` on IndexedDB, as [`super::IndexedDbStore::open_reporting`].
  pub async fn open_indexed_db_reporting(
    name: &str,
    database_key: &SymmetricKey,
    on_progress: impl FnMut(u8) -> ControlFlow<()>,
  ) -> Result<Self, StoreError> {
    let records = IndexedDbRecords::read(name, OpenMode::Upgrade).await?;
    let store = Self::open_backend_to_build(
      BrowserRecords::IndexedDb(records),
      database_key,
      SealedStoreLimits::default(),
    )?;
    build_catalog_committing(&store, indexed_db_records, on_progress).await?;
    Ok(store)
  }

  /// The store `name` on OPFS, in a dedicated worker, as [`super::OpfsStore::open_reporting`]:
  /// moved from IndexedDB the first time.
  pub async fn open_opfs_reporting(
    name: &str,
    database_key: &SymmetricKey,
    on_progress: impl FnMut(u8) -> ControlFlow<()>,
  ) -> Result<(Self, OpfsOpening), StoreError> {
    let (records, opening) = OpfsRecords::open(name).await?;
    let store = Self::open_backend_to_build(
      BrowserRecords::Opfs(records),
      database_key,
      SealedStoreLimits::default(),
    )?;
    build_catalog_yielding(&store, on_progress).await?;
    Ok((store, opening))
  }

  /// Whether the records are on OPFS.
  pub fn is_on_opfs(&self) -> bool {
    matches!(self.backend(), BrowserRecords::Opfs(_))
  }

  /// Makes everything written so far durable; fails with the reason while writes are unsaved.
  /// IndexedDB waits for its transactions; OPFS flushes its files.
  pub async fn flush(&self) -> Result<(), StoreError> {
    match self.backend() {
      BrowserRecords::IndexedDb(records) => records.flush().await,
      BrowserRecords::Opfs(records) => records.flush(),
    }
  }

  /// The page is being hidden, frozen or unloaded: IndexedDB issues what is queued without
  /// waiting; OPFS saves its index (the next start reads no log) and flushes, both synchronous.
  pub fn write_now(&self) {
    match self.backend() {
      BrowserRecords::IndexedDb(records) => records.write_now(),
      BrowserRecords::Opfs(records) => {
        if let Err(error) = records.checkpoint().and_then(|()| records.flush()) {
          tracing::warn!(%error, "could not save the local store before the page went");
        }
      },
    }
  }

  /// Changes with every write: a reader can tell whether anything was written since it last
  /// looked.
  pub fn write_revision(&self) -> u32 {
    match self.backend() {
      BrowserRecords::IndexedDb(records) => records.write_revision(),
      BrowserRecords::Opfs(records) => records.write_revision(),
    }
  }

  /// Whether so many records wait for a failing disk that the owner should pause its sync (issue
  /// #1201).
  pub fn is_backlogged(&self) -> bool {
    match self.backend() {
      BrowserRecords::IndexedDb(records) => records.is_backlogged(),
      BrowserRecords::Opfs(records) => records.is_backlogged(),
    }
  }
}

/// The IndexedDB records of the store, if it is on IndexedDB.
fn indexed_db_records(store: &BrowserStore) -> Option<&IndexedDbRecords> {
  match store.backend() {
    BrowserRecords::IndexedDb(records) => Some(records),
    BrowserRecords::Opfs(_) => None,
  }
}

impl RecordBackend for BrowserRecords {
  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    match self {
      Self::IndexedDb(records) => records.put_record(id, sealed),
      Self::Opfs(records) => records.put_record(id, sealed),
    }
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    match self {
      Self::IndexedDb(records) => records.delete_record(id),
      Self::Opfs(records) => records.delete_record(id),
    }
  }

  fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
    match self {
      Self::IndexedDb(records) => records.get_record(id),
      Self::Opfs(records) => records.get_record(id),
    }
  }

  fn records_after(
    &self,
    maybe_after: Option<&LookupId>,
    limit: usize,
  ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
    match self {
      Self::IndexedDb(records) => records.records_after(maybe_after, limit),
      Self::Opfs(records) => records.records_after(maybe_after, limit),
    }
  }

  fn indexes_words(&self) -> bool {
    match self {
      Self::IndexedDb(records) => records.indexes_words(),
      Self::Opfs(records) => records.indexes_words(),
    }
  }

  fn purge_freed(&self) -> Result<(), StoreError> {
    match self {
      Self::IndexedDb(records) => records.purge_freed(),
      Self::Opfs(records) => records.purge_freed(),
    }
  }

  fn purge_freed_soon(&self) {
    match self {
      Self::IndexedDb(records) => records.purge_freed_soon(),
      Self::Opfs(records) => records.purge_freed_soon(),
    }
  }

  fn wait_for_purge(&self, max_wait: std::time::Duration) -> bool {
    match self {
      Self::IndexedDb(records) => records.wait_for_purge(max_wait),
      Self::Opfs(records) => records.wait_for_purge(max_wait),
    }
  }

  fn before_writes(&self) {
    match self {
      Self::IndexedDb(records) => records.before_writes(),
      Self::Opfs(records) => records.before_writes(),
    }
  }

  fn holds_writes(&self) -> bool {
    match self {
      Self::IndexedDb(records) => records.holds_writes(),
      Self::Opfs(records) => records.holds_writes(),
    }
  }

  fn put_lazy(&self, id: &LookupId, size_hint: usize, seal: LazySeal) -> Result<(), StoreError> {
    match self {
      Self::IndexedDb(records) => records.put_lazy(id, size_hint, seal),
      Self::Opfs(records) => records.put_lazy(id, size_hint, seal),
    }
  }

  fn begin_read(&self) -> Result<(), StoreError> {
    match self {
      Self::IndexedDb(records) => records.begin_read(),
      Self::Opfs(records) => records.begin_read(),
    }
  }

  fn end_read(&self) {
    match self {
      Self::IndexedDb(records) => records.end_read(),
      Self::Opfs(records) => records.end_read(),
    }
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    match self {
      Self::IndexedDb(records) => records.begin_writes(),
      Self::Opfs(records) => records.begin_writes(),
    }
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    match self {
      Self::IndexedDb(records) => records.end_writes(),
      Self::Opfs(records) => records.end_writes(),
    }
  }

  fn fail_group(&self) {
    match self {
      Self::IndexedDb(records) => records.fail_group(),
      Self::Opfs(records) => records.fail_group(),
    }
  }

  fn write_soon(&self) {
    match self {
      Self::IndexedDb(records) => records.write_soon(),
      Self::Opfs(records) => records.write_soon(),
    }
  }

  fn write_tally(&self) -> Option<WriteTally> {
    match self {
      Self::IndexedDb(records) => records.write_tally(),
      Self::Opfs(records) => records.write_tally(),
    }
  }

  fn stage_commit(&self) -> Option<CommitStamp> {
    match self {
      Self::IndexedDb(records) => records.stage_commit(),
      Self::Opfs(records) => records.stage_commit(),
    }
  }

  fn commit_stamp(&self) -> Option<CommitStamp> {
    match self {
      Self::IndexedDb(records) => records.commit_stamp(),
      Self::Opfs(records) => records.commit_stamp(),
    }
  }
}
