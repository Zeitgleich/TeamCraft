//! [`LocalStore`](crate::store::LocalStore) on the origin private file system (issue #2266): a
//! [`SealedStore`] whose records live in a [`RecordLog`] on OPFS sync access handles, read when the
//! store asks for one instead of all at open (e2ee.md §9.1).
//!
//! Sync access handles exist only in dedicated workers, so this store opens only there; the core
//! that uses it runs in one. Each handle locks its file for this worker: a second worker (another
//! tab's core) opening the same store fails with [`StoreError::InUse`]. The tab that owns the store
//! holds the Web Lock first (e2ee.md §9.4), so that is the fallback, not the rule.
//!
//! **From IndexedDB, once.** A store that has no log yet takes the records of the IndexedDB store
//! of the same name ([`super::IndexedDbStore`]), the same sealed bytes under the same IDs, writes
//! them as the log's first group, and only then empties IndexedDB's records. Interrupted before
//! the log's header was written, the next open does it again from IndexedDB; interrupted after, it
//! finds a log and empties IndexedDB's records again. The database key stays where it was,
//! wrapped in the same IndexedDB database (e2ee.md §9.1, point 5): nothing about the key or the
//! sealing changes.
//!
//! Writes reach the files when a group of writes ends (into the browser's file cache, which a tab
//! that crashes doesn't lose); [`OpfsStore::flush`] makes them durable on the disk, as the web core
//! does before every request that changes server state.

use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
  FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetDirectoryOptions, FileSystemGetFileOptions,
  FileSystemReadWriteOptions, FileSystemRemoveOptions, FileSystemSyncAccessHandle, IdbTransactionMode,
  WorkerGlobalScope,
};

use super::idb::{self, OpenMode, RECORDS};
use super::indexed_db::read_all;
use crate::clock::{Clock, SystemClock};
use crate::store::record_log::{LogFile, LogFileError, LogLimits, Opened, RecordLog};
use crate::store::sealed::{BuildStep, RecordBackend, SealedStore};
use crate::store::{SealedStoreLimits, StoreError};

/// The directory in the origin's private file system holding one directory per store.
const STORES_DIRECTORY: &str = "teamcraft-stores";
pub(super) const SLOT_FILES: [&str; 2] = ["records-0.log", "records-1.log"];
pub(super) const INDEX_FILE: &str = "records.idx";
/// `DOMException.name` of a write refused for lack of space.
const QUOTA_EXCEEDED: &str = "QuotaExceededError";
/// `DOMException.name` of a sync access handle another worker holds.
const LOCKED: &str = "NoModificationAllowedError";
/// `DOMException.name` of a file or directory that doesn't exist.
const NOT_FOUND: &str = "NotFoundError";

/// The encrypted browser store on OPFS.
pub type OpfsStore = SealedStore<OpfsRecords>;

/// The sealed records in their log.
pub struct OpfsRecords {
  log: RefCell<RecordLog<OpfsFile>>,
  /// Counts the writes, for [`OpfsRecords::write_revision`].
  writes: Cell<u32>,
}

/// One file of the log, through its sync access handle; closed when dropped.
pub(crate) struct OpfsFile {
  handle: FileSystemSyncAccessHandle,
}

/// How the store was opened, for the start's timing and logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpfsOpening {
  /// From its log.
  Opened,
  /// Made new, with this many records taken from IndexedDB.
  Migrated { records: usize },
}

impl SealedStore<OpfsRecords> {
  /// Opens the store `name` with `database_key` in a dedicated worker, taking it over from
  /// IndexedDB the first time; builds the catalog of a store written before it existed.
  pub async fn open(name: &str, database_key: &SymmetricKey) -> Result<(Self, OpfsOpening), StoreError> {
    Self::open_reporting(name, database_key, |_| ControlFlow::Continue(())).await
  }

  /// [`Self::open`] for a page that shows how far a catalog build is; `on_progress` may stop it
  /// ([`StoreError::InUse`]), as for the IndexedDB store.
  pub async fn open_reporting(
    name: &str,
    database_key: &SymmetricKey,
    on_progress: impl FnMut(u8) -> ControlFlow<()>,
  ) -> Result<(Self, OpfsOpening), StoreError> {
    let (backend, opening) = OpfsRecords::open(name).await?;
    let store = Self::open_backend_to_build(backend, database_key, SealedStoreLimits::default())?;
    build_catalog_yielding(&store, on_progress).await?;
    Ok((store, opening))
  }

  /// Deletes the store `name`: its log and what IndexedDB holds of it (records and key wrapping).
  /// The store must be closed first. Works in the page too (sign-out, a reset): a browser without
  /// OPFS there holds no log to delete.
  pub async fn delete(name: &str) -> Result<(), StoreError> {
    if let Some(stores) = existing_stores_directory().await? {
      let options = FileSystemRemoveOptions::new();
      options.set_recursive(true);
      match JsFuture::from(stores.remove_entry_with_options(name, &options)).await {
        Ok(_) => {},
        Err(error) if dom_name(&error).as_deref() == Some(NOT_FOUND) => {},
        Err(error) => return Err(opfs_error("delete", &error)),
      }
    }
    idb::delete_database(name).await
  }

  /// Writes again what the disk refused, makes the log durable, and fails with the reason while
  /// writes are still unsaved (the app shows that changes are not saved on this device).
  pub fn flush(&self) -> Result<(), StoreError> {
    self.backend().flush()
  }

  /// Saves the index, so the next start reads no log: when the page is hidden or closed.
  pub fn checkpoint(&self) -> Result<(), StoreError> {
    self.backend().checkpoint()
  }

  /// Whether so many records wait for a disk that refuses them that the owner should pause its
  /// sync (issue #1201), as [`super::IndexedDbStore::is_backlogged`].
  pub fn is_backlogged(&self) -> bool {
    self.backend().is_backlogged()
  }

  /// Whether OPFS holds a log of the store `name`, so its records moved out of IndexedDB (issue
  /// #2363): from the page too, creating nothing.
  pub async fn has_log(name: &str) -> Result<bool, StoreError> {
    super::service_worker::has_log(name).await
  }

  /// Whether OPFS holds the files of a log of the store `name`, without reading them: a page that
  /// asks while the worker of the page before it is still letting go of them (a reload, a
  /// navigation) must not read them. Reading them then crashed Chrome's renderer (#2397). A log
  /// whose files exist but hold nothing yet is the worker's to take over, which opens it and moves
  /// the records from IndexedDB, as on the first start.
  pub async fn has_log_files(name: &str) -> Result<bool, StoreError> {
    super::service_worker::has_log_files(name).await
  }
}

/// Runs the first catalog build of `store` with a yield between batches: the worker's thread is
/// the core's own, so its messages get in between. `on_progress` may stop it
/// ([`StoreError::InUse`]).
pub(super) async fn build_catalog_yielding<B: RecordBackend>(
  store: &SealedStore<B>,
  mut on_progress: impl FnMut(u8) -> ControlFlow<()>,
) -> Result<(), StoreError> {
  let Some(mut build) = store.start_catalog_build()? else {
    return Ok(());
  };
  loop {
    let step = store.step_catalog_build(&mut build)?;
    crate::platform::sleep(std::time::Duration::ZERO).await;
    if step == BuildStep::Complete {
      return Ok(());
    }
    if on_progress(build.percent()).is_break() {
      return Err(StoreError::InUse);
    }
  }
}

impl RecordBackend for OpfsRecords {
  /// No tantivy in the browser: its search reads the word index (issue #1833).
  fn indexes_words(&self) -> bool {
    true
  }

  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    self.log.borrow_mut().put(id, sealed, SystemClock.now_ms());
    self.writes.set(self.writes.get().wrapping_add(1));
    Ok(())
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    self.log.borrow_mut().delete(id, SystemClock.now_ms());
    self.writes.set(self.writes.get().wrapping_add(1));
    Ok(())
  }

  fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
    self.log.borrow().get(id).map_err(store_error)
  }

  fn records_after(
    &self,
    maybe_after: Option<&LookupId>,
    limit: usize,
  ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
    self
      .log
      .borrow()
      .records_after(maybe_after, limit)
      .map_err(store_error)
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    self.log.borrow_mut().begin_group();
    Ok(())
  }

  /// Writes the outermost group. A write the disk refuses stays in memory and goes again with the
  /// next group: the group ends fine, and the flush reports it (the IndexedDB store's semantics).
  fn end_writes(&self) -> Result<(), StoreError> {
    self.log.borrow_mut().end_group(SystemClock.now_ms());
    Ok(())
  }
}

impl OpfsRecords {
  /// Opens the log of the store `name`, or makes it from the IndexedDB store of that name.
  pub(super) async fn open(name: &str) -> Result<(Self, OpfsOpening), StoreError> {
    let started_ms = js_sys::Date::now();
    let directory = store_directory(name).await?;
    let slots = [
      OpfsFile::open(&directory, SLOT_FILES[0]).await?,
      OpfsFile::open(&directory, SLOT_FILES[1]).await?,
    ];
    let index = OpfsFile::open(&directory, INDEX_FILE).await?;
    let database = idb::open_database_with(name, OpenMode::Upgrade).await?;
    let opened = match RecordLog::open(slots, index, LogLimits::default()) {
      Ok(Opened::Log(log)) => Ok((log, OpfsOpening::Opened)),
      Ok(Opened::Empty { slots, index_file }) => migrate(slots, index_file, &database).await,
      Err(error) => Err(store_error(error)),
    };
    let opened = match opened {
      Ok(opened) => opened,
      Err(error) => {
        database.close();
        return Err(error);
      },
    };
    // Also after a migration interrupted between the log and this: IndexedDB's copy is stale.
    let cleared = clear_records(&database).await;
    database.close();
    if let Err(error) = cleared {
      tracing::warn!(%error, "the local store's old IndexedDB records could not be removed");
    }
    let (log, opening) = opened;
    let (records, log_bytes) = log.sizes();
    tracing::info!(
      records,
      log_mib = log_bytes / (1024 * 1024),
      open_ms = js_sys::Date::now() - started_ms,
      ?opening,
      wasm_mib = core::arch::wasm32::memory_size(0) / 16,
      "opened the local store"
    );
    Ok((
      Self {
        log: RefCell::new(log),
        writes: Cell::new(0),
      },
      opening,
    ))
  }

  /// See [`OpfsStore::flush`].
  pub(super) fn flush(&self) -> Result<(), StoreError> {
    self
      .log
      .borrow_mut()
      .flush(SystemClock.now_ms())
      .map_err(store_error)
  }

  /// See [`OpfsStore::checkpoint`].
  pub(super) fn checkpoint(&self) -> Result<(), StoreError> {
    self.log.borrow_mut().checkpoint().map_err(store_error)
  }

  /// See [`OpfsStore::is_backlogged`].
  pub(super) fn is_backlogged(&self) -> bool {
    self.log.borrow().is_backlogged()
  }

  /// Changes with every write, as [`super::IndexedDbStore::write_revision`].
  pub(super) fn write_revision(&self) -> u32 {
    self.writes.get()
  }
}

impl OpfsFile {
  /// Opens `name` in `directory` (created if missing) with a sync access handle.
  async fn open(directory: &FileSystemDirectoryHandle, name: &str) -> Result<Self, StoreError> {
    let options = FileSystemGetFileOptions::new();
    options.set_create(true);
    let file: FileSystemFileHandle = JsFuture::from(directory.get_file_handle_with_options(name, &options))
      .await
      .map_err(|error| opfs_error("file", &error))?
      .unchecked_into();
    let handle = JsFuture::from(file.create_sync_access_handle())
      .await
      .map_err(|error| match dom_name(&error).as_deref() {
        Some(LOCKED) => StoreError::InUse,
        _ => opfs_error("lock", &error),
      })?
      .unchecked_into();
    Ok(Self { handle })
  }
}

impl LogFile for OpfsFile {
  fn size(&self) -> Result<u64, LogFileError> {
    let size = self.handle.get_size().map_err(file_error)?;
    // A file size in JS is an integer below 2^53.
    Ok(size as u64)
  }

  fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<usize, LogFileError> {
    let options = FileSystemReadWriteOptions::new();
    options.set_at(offset as f64);
    let read = self
      .handle
      .read_with_u8_array_and_options(buffer, &options)
      .map_err(file_error)?;
    Ok(read as usize)
  }

  fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), LogFileError> {
    let options = FileSystemReadWriteOptions::new();
    options.set_at(offset as f64);
    let written = self
      .handle
      .write_with_u8_array_and_options(data, &options)
      .map_err(file_error)?;
    if (written as usize) < data.len() {
      // The browser wrote less than asked: the disk is full.
      return Err(LogFileError::QuotaExceeded);
    }
    Ok(())
  }

  fn truncate(&self, size: u64) -> Result<(), LogFileError> {
    self.handle.truncate_with_f64(size as f64).map_err(file_error)
  }

  fn flush(&self) -> Result<(), LogFileError> {
    self.handle.flush().map_err(file_error)
  }
}

impl Drop for OpfsFile {
  fn drop(&mut self) {
    // Unsaved writes stay in the browser's file cache; closing releases the file's lock.
    if let Err(error) = self.handle.flush() {
      tracing::debug!(error = %crate::platform::describe_js_error(&error), "flushing a closing log file failed");
    }
    self.handle.close();
  }
}

/// Makes the log from the IndexedDB store's records (none for a new store).
async fn migrate(
  slots: [OpfsFile; 2],
  index_file: OpfsFile,
  database: &web_sys::IdbDatabase,
) -> Result<(RecordLog<OpfsFile>, OpfsOpening), StoreError> {
  let records = read_all(database).await?;
  let count = records.len();
  let log = RecordLog::create(slots, index_file, LogLimits::default(), records).map_err(store_error)?;
  if count > 0 {
    tracing::info!(records = count, "moved the local store from IndexedDB to OPFS");
  }
  Ok((log, OpfsOpening::Migrated { records: count }))
}

/// Empties IndexedDB's records of the store, if it holds any.
async fn clear_records(database: &web_sys::IdbDatabase) -> Result<(), StoreError> {
  let transaction = database
    .transaction_with_str_and_mode(RECORDS, IdbTransactionMode::Readwrite)
    .map_err(|error| idb::js_error("clear", &error))?;
  let records = transaction
    .object_store(RECORDS)
    .map_err(|error| idb::js_error("clear", &error))?;
  let count = records.count().map_err(|error| idb::js_error("clear", &error))?;
  let count = idb::request_result(&count).await?.as_f64().unwrap_or(0.0);
  if count > 0.0 {
    records.clear().map_err(|error| idb::js_error("clear", &error))?;
  }
  idb::transaction_done(&transaction).await
}

/// The directory of the store `name`, created if missing.
async fn store_directory(name: &str) -> Result<FileSystemDirectoryHandle, StoreError> {
  let stores = stores_directory().await?;
  directory(&stores, name).await
}

async fn stores_directory() -> Result<FileSystemDirectoryHandle, StoreError> {
  if js_sys::global().dyn_into::<WorkerGlobalScope>().is_err() {
    return Err(StoreError::Database(
      "the OPFS store opens only in a worker".into(),
    ));
  }
  directory(&storage_root().await?, STORES_DIRECTORY).await
}

/// The directory of the store `name` if it exists; creates nothing (the service worker reads
/// only, e2ee.md §9.3).
pub(super) async fn existing_store_directory(
  name: &str,
) -> Result<Option<FileSystemDirectoryHandle>, StoreError> {
  let Some(stores) = existing_stores_directory().await? else {
    return Ok(None);
  };
  existing_directory(&stores, name).await
}

/// The directory holding the stores if it exists. `None` also where the browser offers no OPFS
/// (a private window of some browsers): no store can be there.
async fn existing_stores_directory() -> Result<Option<FileSystemDirectoryHandle>, StoreError> {
  let root = match storage_root().await {
    Ok(root) => root,
    Err(error) => {
      tracing::debug!(%error, "no origin private file system");
      return Ok(None);
    },
  };
  existing_directory(&root, STORES_DIRECTORY).await
}

async fn existing_directory(
  parent: &FileSystemDirectoryHandle,
  name: &str,
) -> Result<Option<FileSystemDirectoryHandle>, StoreError> {
  match JsFuture::from(parent.get_directory_handle(name)).await {
    Ok(handle) => Ok(Some(handle.unchecked_into())),
    Err(error) if dom_name(&error).as_deref() == Some(NOT_FOUND) => Ok(None),
    Err(error) => Err(opfs_error("directory", &error)),
  }
}

/// The origin's private file system: from a worker of any kind or the page.
async fn storage_root() -> Result<FileSystemDirectoryHandle, StoreError> {
  let global = js_sys::global();
  let storage = if let Some(scope) = global.dyn_ref::<WorkerGlobalScope>() {
    scope.navigator().storage()
  } else if let Some(window) = global.dyn_ref::<web_sys::Window>() {
    window.navigator().storage()
  } else {
    return Err(StoreError::Database("no storage in this scope".into()));
  };
  Ok(
    JsFuture::from(storage.get_directory())
      .await
      .map_err(|error| opfs_error("root", &error))?
      .unchecked_into(),
  )
}

async fn directory(
  parent: &FileSystemDirectoryHandle,
  name: &str,
) -> Result<FileSystemDirectoryHandle, StoreError> {
  let options = FileSystemGetDirectoryOptions::new();
  options.set_create(true);
  Ok(
    JsFuture::from(parent.get_directory_handle_with_options(name, &options))
      .await
      .map_err(|error| opfs_error("directory", &error))?
      .unchecked_into(),
  )
}

pub(super) fn store_error(error: LogFileError) -> StoreError {
  match error {
    LogFileError::QuotaExceeded => StoreError::QuotaExceeded,
    LogFileError::Closed => StoreError::Closed,
    LogFileError::Io(message) => StoreError::Database(format!("OPFS: {message}")),
  }
}

fn file_error(error: JsValue) -> LogFileError {
  match dom_name(&error).as_deref() {
    Some(QUOTA_EXCEEDED) => LogFileError::QuotaExceeded,
    Some("InvalidStateError") => LogFileError::Closed,
    _ => LogFileError::Io(crate::platform::describe_js_error(&error)),
  }
}

pub(super) fn opfs_error(context: &'static str, error: &JsValue) -> StoreError {
  StoreError::Database(format!(
    "OPFS {context}: {}",
    crate::platform::describe_js_error(error)
  ))
}

pub(super) fn dom_name(error: &JsValue) -> Option<String> {
  error
    .dyn_ref::<web_sys::DomException>()
    .map(web_sys::DomException::name)
}
