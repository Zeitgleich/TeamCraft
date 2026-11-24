//! The store as the browser's service worker opens it (e2ee.md §9.3, issue #2266): for a push's
//! preview, the "mark as read" button and a rotated push subscription.
//!
//! The service worker has no sync access handles, so it can't open the store on OPFS
//! ([`super::OpfsStore`]) the way its owner does. Where the store is on OPFS it reads a copy of the
//! log through `getFile()`, which needs no lock: the header of both slots, then the whole slot that
//! holds the log and the index file. That is the same as the IndexedDB store, whose open reads
//! every record's ciphertext. The copy never writes: [`StoreError::ReadOnly`] for every write, and
//! a group cut off as incomplete is cut off the copy only, not the file the owner writes.
//!
//! Where no log is on OPFS (a store not moved yet, or a move interrupted before the log's header
//! was written) the records are in IndexedDB, opened as before. Once a log exists, IndexedDB's
//! records are stale or gone and are not read. The database key stays in IndexedDB either way.
//!
//! The owner may write while the copy is taken: the copy is the log as it was, cut at its last
//! complete group. A compaction between reading the headers and copying the slot leaves a copy
//! without a log; the copy is then taken again.

use std::cell::RefCell;

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, FileSystemDirectoryHandle, FileSystemFileHandle};

use super::idb::OpenMode;
use super::indexed_db::IndexedDbRecords;
use super::opfs::{INDEX_FILE, SLOT_FILES, dom_name, existing_store_directory, opfs_error, store_error};
use crate::store::record_log::{CopiedFile, LogLimits, Opened, RecordLog, SLOT_HEADER_BYTES, newest_slot};
use crate::store::sealed::{LazySeal, RecordBackend, SealedStore};
use crate::store::{SealedStoreLimits, StoreError};

/// How often the copy is taken again when the owner compacted the log while it was copied.
const COPY_ATTEMPTS: usize = 3;

/// The store in the service worker: its records from OPFS or IndexedDB, wherever they are.
pub type ServiceWorkerStore = SealedStore<ServiceWorkerRecords>;

/// Where the service worker found the store's records.
pub enum ServiceWorkerRecords {
  /// Not moved to OPFS yet: IndexedDB, opened at its version, never upgraded.
  IndexedDb(IndexedDbRecords),
  /// A copy of the log on OPFS.
  Opfs(OpfsCopy),
}

/// A read-only copy of the store's log on OPFS.
pub struct OpfsCopy {
  log: RefCell<RecordLog<CopiedFile>>,
}

impl SealedStore<ServiceWorkerRecords> {
  /// Opens the store `name` with `database_key` for the service worker, a reader beside the page
  /// that owns it: never upgrading IndexedDB or building the catalog, and never writing OPFS.
  pub async fn open_for_service_worker(name: &str, database_key: &SymmetricKey) -> Result<Self, StoreError> {
    let backend = ServiceWorkerRecords::read(name).await?;
    Self::open_backend_without_building(backend, database_key, SealedStoreLimits::default())
  }

  /// Whether the records came from OPFS, for tests and logs.
  pub fn is_on_opfs(&self) -> bool {
    matches!(self.backend(), ServiceWorkerRecords::Opfs(_))
  }
}

impl ServiceWorkerRecords {
  async fn read(name: &str) -> Result<Self, StoreError> {
    if let Some(copy) = OpfsCopy::read(name).await? {
      return Ok(Self::Opfs(copy));
    }
    Ok(Self::IndexedDb(
      IndexedDbRecords::read(name, OpenMode::AsIs).await?,
    ))
  }

  fn indexed_db_or_refuse(&self) -> Result<&IndexedDbRecords, StoreError> {
    match self {
      Self::IndexedDb(records) => Ok(records),
      Self::Opfs(_) => Err(StoreError::ReadOnly),
    }
  }
}

impl RecordBackend for ServiceWorkerRecords {
  fn indexes_words(&self) -> bool {
    match self {
      Self::IndexedDb(records) => records.indexes_words(),
      Self::Opfs(_) => true,
    }
  }

  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    self.indexed_db_or_refuse()?.put_record(id, sealed)
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    self.indexed_db_or_refuse()?.delete_record(id)
  }

  fn put_lazy(&self, id: &LookupId, size_hint: usize, seal: LazySeal) -> Result<(), StoreError> {
    self.indexed_db_or_refuse()?.put_lazy(id, size_hint, seal)
  }

  fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
    match self {
      Self::IndexedDb(records) => records.get_record(id),
      Self::Opfs(copy) => copy.log.borrow().get(id).map_err(store_error),
    }
  }

  fn records_after(
    &self,
    maybe_after: Option<&LookupId>,
    limit: usize,
  ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
    match self {
      Self::IndexedDb(records) => records.records_after(maybe_after, limit),
      Self::Opfs(copy) => copy
        .log
        .borrow()
        .records_after(maybe_after, limit)
        .map_err(store_error),
    }
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    self.indexed_db_or_refuse()?.begin_writes()
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    self.indexed_db_or_refuse()?.end_writes()
  }

  fn write_soon(&self) {
    if let Self::IndexedDb(records) = self {
      records.write_soon();
    }
  }
}

impl OpfsCopy {
  /// A copy of the log of the store `name`; `None` when OPFS holds no log of it (its records are
  /// in IndexedDB). Creates nothing on OPFS.
  async fn read(name: &str) -> Result<Option<Self>, StoreError> {
    let started_ms = js_sys::Date::now();
    let Some(directory) = existing_store_directory(name).await? else {
      return Ok(None);
    };
    let Some(files) = LogFiles::find(&directory).await? else {
      return Ok(None);
    };
    for _ in 0..COPY_ATTEMPTS {
      let log = match files.copy().await? {
        Copied::Log(log) => log,
        Copied::NoLog => return Ok(None),
        Copied::Moved => {
          tracing::debug!("the local store's log moved while it was copied; copying it again");
          continue;
        },
      };
      let (records, log_bytes) = log.sizes();
      tracing::info!(
        records,
        log_mib = log_bytes / (1024 * 1024),
        read_ms = js_sys::Date::now() - started_ms,
        "read a copy of the local store"
      );
      return Ok(Some(Self {
        log: RefCell::new(log),
      }));
    }
    Err(StoreError::Database(
      "the local store's log kept moving while it was copied".into(),
    ))
  }
}

/// Whether OPFS holds a log of the store `name`: its records moved there (issue #2363). Reads the
/// slots' headers only and creates nothing, so the page can ask before it decides where the core
/// runs. A slot it can't read (a worker holds it) counts as a log: the move is never undone by
/// guessing wrong.
pub(super) async fn has_log(name: &str) -> Result<bool, StoreError> {
  let Some(directory) = existing_store_directory(name).await? else {
    return Ok(false);
  };
  let Some(files) = LogFiles::find(&directory).await? else {
    return Ok(false);
  };
  let read = |slot| read_file(slot, Some(SLOT_HEADER_BYTES));
  let (first, second) = match (read(&files.slots[0]).await, read(&files.slots[1]).await) {
    (Ok(first), Ok(second)) => (first, second),
    (Err(error), _) | (_, Err(error)) => {
      tracing::debug!(%error, "a slot of the local store's log can't be read; taking it as moved");
      return Ok(true);
    },
  };
  let headers = [CopiedFile::new(first), CopiedFile::new(second)];
  Ok(newest_slot(&headers).map_err(store_error)?.is_some())
}

/// The handles of a store's log files on OPFS.
struct LogFiles {
  slots: [FileSystemFileHandle; 2],
  /// A missing index costs a scan of the log, like a damaged one.
  maybe_index: Option<FileSystemFileHandle>,
}

/// What copying the log found.
enum Copied {
  Log(RecordLog<CopiedFile>),
  /// Neither slot holds a log: the records are in IndexedDB.
  NoLog,
  /// The slot the headers named no longer held the log once it was copied (a compaction
  /// meanwhile): copy again.
  Moved,
}

impl LogFiles {
  /// The files of the log in `directory`; `None` when a slot is missing (no log was ever made).
  async fn find(directory: &FileSystemDirectoryHandle) -> Result<Option<Self>, StoreError> {
    let (Some(first), Some(second)) = (
      existing_file(directory, SLOT_FILES[0]).await?,
      existing_file(directory, SLOT_FILES[1]).await?,
    ) else {
      return Ok(None);
    };
    Ok(Some(Self {
      slots: [first, second],
      maybe_index: existing_file(directory, INDEX_FILE).await?,
    }))
  }

  /// The log from copies of the files: the headers of both slots, then the whole of the one that
  /// holds the log, and the index.
  async fn copy(&self) -> Result<Copied, StoreError> {
    let headers = [
      CopiedFile::new(read_file(&self.slots[0], Some(SLOT_HEADER_BYTES)).await?),
      CopiedFile::new(read_file(&self.slots[1], Some(SLOT_HEADER_BYTES)).await?),
    ];
    let Some(active) = newest_slot(&headers).map_err(store_error)? else {
      return Ok(Copied::NoLog);
    };
    let [first, second] = headers;
    let whole = CopiedFile::new(read_file(&self.slots[active], None).await?);
    let index = match &self.maybe_index {
      Some(index) => CopiedFile::new(read_file(index, None).await?),
      None => CopiedFile::new(Vec::new()),
    };
    let slots = if active == 0 {
      [whole, second]
    } else {
      [first, whole]
    };
    // Only the slot copied whole can hold the log the copy reads: of the other one only the header
    // was copied, so a log found there (a leftover header) would read nothing.
    match RecordLog::open(slots, index, LogLimits::default()).map_err(store_error)? {
      Opened::Log(log) if log.active_slot() == active => Ok(Copied::Log(log)),
      _ => Ok(Copied::Moved),
    }
  }
}

/// The file `name` in `directory`, if it exists.
async fn existing_file(
  directory: &FileSystemDirectoryHandle,
  name: &str,
) -> Result<Option<FileSystemFileHandle>, StoreError> {
  match JsFuture::from(directory.get_file_handle(name)).await {
    Ok(handle) => Ok(Some(handle.unchecked_into())),
    Err(error) if dom_name(&error).as_deref() == Some("NotFoundError") => Ok(None),
    Err(error) => Err(opfs_error("file", &error)),
  }
}

/// The file's first `maybe_len` bytes, or all of it.
async fn read_file(handle: &FileSystemFileHandle, maybe_len: Option<u64>) -> Result<Vec<u8>, StoreError> {
  let file: Blob = JsFuture::from(handle.get_file())
    .await
    .map_err(|error| opfs_error("read", &error))?
    .unchecked_into();
  let blob = match maybe_len {
    // A file size in JS is an integer below 2^53.
    Some(len) if (len as f64) < file.size() => file
      .slice_with_f64_and_f64(0.0, len as f64)
      .map_err(|error| opfs_error("read", &error))?,
    _ => file,
  };
  let buffer = JsFuture::from(blob.array_buffer())
    .await
    .map_err(|error| opfs_error("read", &error))?;
  Ok(js_sys::Uint8Array::new(&buffer).to_vec())
}
