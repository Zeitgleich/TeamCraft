//! [`LocalStore`] on IndexedDB: a [`SealedStore`] whose records live in one object store
//! (e2ee.md §9.1).
//!
//! [`LocalStore`] is synchronous, IndexedDB is not. Opening reads every record; writes are queued
//! and issued together in one transaction a moment later ([`BATCH_DELAY`], or at once when much is
//! queued or on [`IndexedDbStore::flush`]). Of several writes of one record in that time only the
//! last is issued: a catch-up rewrites some records (the MLS state) after every group it processes,
//! and one transaction per write left the browser writing for minutes after the catch-up was done
//! (docs/performance.md). IndexedDB runs read-write transactions on the same object store in the
//! order they were created, so the database follows the cache in order.
//! [`IndexedDbStore::flush`] waits until everything written so far is committed.
//!
//! Durability: a batch is one transaction, so what survives a page that dies is always a prefix of
//! the writes in their order (never a cursor or MLS state without the messages written before it).
//! What must not be lost is made durable before it is confirmed: the web core's API waits for a
//! flush before every request that changes server state, and the page issues the queue at once when
//! it is hidden, frozen or unloaded ([`IndexedDbStore::write_now`]). A write lost anyway is one the
//! server never heard of; the next start catches up from the last persisted cursor.
//!
//! A group of writes ([`crate::store::LocalStore::begin_writes`]) is never split between two
//! transactions: it runs synchronously, so the batch timer and the page's lifecycle events can't
//! issue in the middle of it, and the size limit waits until the group ended.
//!
//! A write that fails (quota exceeded, the database closed for a newer version) is kept and written
//! again on the next flush, so the database catches up with the cache once space is free again
//! instead of silently rolling back on the next reload (e2ee.md §9.4). The flush reports the
//! failure; the app shows it to the user.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbObjectStore, IdbTransaction, IdbTransactionMode};

use super::idb::{self, RECORDS};
use crate::store::StoreError;
use crate::store::sealed::{RecordBackend, SealedStore, format_id, parse_id};

/// How long writes are collected before they are issued together. Short enough that little is
/// lost when the page dies, long enough that a burst of writes becomes one transaction.
const BATCH_DELAY: Duration = Duration::from_millis(50);
/// Queued this much, the writes are issued right away instead of waiting for the batch delay.
const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
/// Property on a write transaction naming the batch of writes it carries.
const BATCH_PROPERTY: &str = "teamcraftBatch";
/// `DOMException.name` of a write refused for lack of space.
const QUOTA_EXCEEDED: &str = "QuotaExceededError";

/// The encrypted browser store.
pub type IndexedDbStore = SealedStore<IndexedDbRecords>;

/// The IndexedDB object store holding the sealed records.
pub struct IndexedDbRecords {
  inner: Rc<Inner>,
}

/// What the batch timer needs to issue the queued writes.
struct Inner {
  database: IdbDatabase,
  writes: Rc<RefCell<Writes>>,
  on_complete: Closure<dyn FnMut(web_sys::Event)>,
  on_abort: Closure<dyn FnMut(web_sys::Event)>,
  _on_version_change: Closure<dyn FnMut(web_sys::Event)>,
}

/// Writes that have not committed yet, by record key.
#[derive(Default)]
struct Writes {
  next_sequence: u32,
  open: HashMap<String, OpenWrite>,
  /// Keys whose latest write waits for the next batch, each once.
  queued: Vec<String>,
  queued_bytes: usize,
  /// A batch timer is running.
  is_scheduled: bool,
  next_batch: u32,
  /// The writes each issued transaction carries, by batch number.
  batches: HashMap<u32, Vec<(String, u32)>>,
  maybe_last_failure: Option<WriteFailure>,
  /// Another page (a newer version) asked for the database; it was closed, writes wait for a reload.
  is_closed: bool,
  /// Depth of the open group of writes; nothing is issued while it is above zero.
  group_depth: u32,
}

/// The latest write of one record. Its value is kept until it committed, to write it again.
struct OpenWrite {
  sequence: u32,
  /// `None` deletes the record.
  maybe_value: Option<Vec<u8>>,
  state: WriteState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteState {
  /// Waits for the next batch.
  Queued,
  /// In a transaction that has not finished.
  Issued,
  /// Refused or aborted; issued again by the next flush.
  Failed,
}

#[derive(Clone, Copy)]
enum WriteFailure {
  QuotaExceeded,
  Closed,
  Other,
}

impl SealedStore<IndexedDbRecords> {
  /// Opens the database `name` and decrypts it into memory with `database_key`.
  pub async fn open(name: &str, database_key: &SymmetricKey) -> Result<Self, StoreError> {
    let database = idb::open_database(name).await?;
    let records = read_all(&database).await?;
    let backend = IndexedDbRecords::new(database);
    Self::load(backend, database_key, records)
  }

  /// Deletes the database `name` (records and key wrapping): the user chose to set up a device
  /// whose data can't be read again (issue #160). The store must be closed first.
  pub async fn delete(name: &str) -> Result<(), StoreError> {
    idb::delete_database(name).await
  }

  /// Issues what is queued and waits until every write issued so far is committed. Writes that
  /// failed are issued again, and the flush fails with the reason, until they went through.
  pub async fn flush(&self) -> Result<(), StoreError> {
    let inner = &self.backend().inner;
    inner.retry_failed();
    inner.issue_queued();
    if !inner.writes.borrow().is_closed {
      // A read-only transaction over the same store starts only after earlier read-write ones ended.
      let transaction = inner
        .database
        .transaction_with_str(RECORDS)
        .map_err(|error| idb::js_error("flush", &error))?;
      idb::transaction_done(&transaction).await?;
    }
    inner.failure()
  }

  /// Issues what is queued now instead of after the batch delay, without waiting for it (the page
  /// is being hidden, frozen or unloaded).
  pub fn write_now(&self) {
    self.backend().inner.issue_queued();
  }

  /// Changes with every write: a reader can tell whether anything was written since it last
  /// looked without comparing contents.
  pub fn write_revision(&self) -> u32 {
    self.backend().inner.writes.borrow().next_sequence
  }
}

impl RecordBackend for IndexedDbRecords {
  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    self.write(format_id(id), Some(sealed.to_vec()));
    Ok(())
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    self.write(format_id(id), None);
    Ok(())
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    let mut writes = self.inner.writes.borrow_mut();
    writes.group_depth = writes.group_depth.saturating_add(1);
    Ok(())
  }

  /// The group is complete: what it queued goes out with the next batch (now, if much is queued).
  fn end_writes(&self) -> Result<(), StoreError> {
    let is_due = {
      let mut writes = self.inner.writes.borrow_mut();
      writes.group_depth = writes.group_depth.saturating_sub(1);
      writes.group_depth == 0 && writes.queued_bytes >= MAX_QUEUED_BYTES
    };
    if is_due {
      self.inner.issue_queued();
    }
    Ok(())
  }

  fn write_soon(&self) {
    if self.inner.writes.borrow().group_depth == 0 {
      self.inner.issue_queued();
    }
  }
}

impl IndexedDbRecords {
  fn new(database: IdbDatabase) -> Self {
    let writes = Rc::new(RefCell::new(Writes::default()));
    let on_complete = Closure::<dyn FnMut(web_sys::Event)>::new({
      let writes = writes.clone();
      move |event: web_sys::Event| {
        if let Some(batch) = batch_of(&event) {
          writes.borrow_mut().committed(batch);
        }
      }
    });
    let on_abort = Closure::<dyn FnMut(web_sys::Event)>::new({
      let writes = writes.clone();
      move |event: web_sys::Event| {
        let failure = event
          .target()
          .and_then(|target| target.dyn_into::<IdbTransaction>().ok())
          .and_then(|transaction| transaction.error())
          .map_or(WriteFailure::Other, |error| classify(&error.name()));
        tracing::error!(
          quota = matches!(failure, WriteFailure::QuotaExceeded),
          "an IndexedDB write failed"
        );
        if let Some(batch) = batch_of(&event) {
          writes.borrow_mut().aborted(batch, failure);
        }
      }
    });
    // e2ee.md §9.4: a newer version of the app (another tab, the service worker) upgrades the
    // database. Holding it open would block that page; this page stops writing and asks for a reload.
    let on_version_change = Closure::<dyn FnMut(web_sys::Event)>::new({
      let writes = writes.clone();
      let database = database.clone();
      move |_| {
        tracing::warn!("a newer version needs the local database; closing it");
        database.close();
        let mut writes = writes.borrow_mut();
        writes.is_closed = true;
        writes.maybe_last_failure = Some(WriteFailure::Closed);
      }
    });
    database.set_onversionchange(Some(on_version_change.as_ref().unchecked_ref()));
    Self {
      inner: Rc::new(Inner {
        database,
        writes,
        on_complete,
        on_abort,
        _on_version_change: on_version_change,
      }),
    }
  }

  /// Queues the write; the batch goes out after [`BATCH_DELAY`], or now when much is queued.
  fn write(&self, key: String, maybe_value: Option<Vec<u8>>) {
    let is_due = {
      let mut writes = self.inner.writes.borrow_mut();
      writes.queue(key, maybe_value);
      if writes.queued_bytes >= MAX_QUEUED_BYTES && writes.group_depth == 0 {
        true
      } else {
        if !writes.is_scheduled {
          writes.is_scheduled = true;
          schedule(Rc::downgrade(&self.inner));
        }
        false
      }
    };
    if is_due {
      self.inner.issue_queued();
    }
  }
}

impl Inner {
  /// Issues every queued write in one transaction. A write that can't even start counts as failed.
  fn issue_queued(&self) {
    // Writes that failed go with every batch: a later write never commits without them, so what
    // is on disk stays a prefix of what was written.
    if !self.writes.borrow().queued.is_empty() {
      self.retry_failed();
    }
    let keys = {
      let mut writes = self.writes.borrow_mut();
      writes.is_scheduled = false;
      writes.queued_bytes = 0;
      std::mem::take(&mut writes.queued)
    };
    if keys.is_empty() {
      return;
    }
    let (transaction, records) = match self.transaction() {
      Ok(opened) => opened,
      Err(failure) => {
        let mut writes = self.writes.borrow_mut();
        for key in &keys {
          writes.fail_queued(key, failure);
        }
        return;
      },
    };
    let mut writes = self.writes.borrow_mut();
    let mut carried = Vec::with_capacity(keys.len());
    let mut maybe_refused = None;
    for key in &keys {
      let key = key.clone();
      let Some(write) = writes
        .open
        .get(&key)
        .filter(|write| write.state == WriteState::Queued)
      else {
        continue;
      };
      let sequence = write.sequence;
      let record = JsValue::from_str(&key);
      let issued = match &write.maybe_value {
        Some(value) => records.put_with_key(&js_sys::Uint8Array::from(value.as_slice()), &record),
        None => records.delete(&record),
      };
      match issued {
        Ok(_) => {
          if let Some(write) = writes.open.get_mut(&key) {
            write.state = WriteState::Issued;
          }
          carried.push((key, sequence));
        },
        // Refused at once (a full disk can refuse at once): nothing of the batch may commit.
        Err(error) => {
          maybe_refused = Some(failure_of(&error));
          break;
        },
      }
    }
    if let Some(failure) = maybe_refused {
      // Aborted before it carried a batch number: the abort handler leaves it alone.
      let _ = transaction.abort();
      for key in &keys {
        writes.fail_open(key, failure);
      }
      return;
    }
    writes.next_batch = writes.next_batch.wrapping_add(1);
    let batch = writes.next_batch;
    writes.batches.insert(batch, carried);
    // The events fire after this returns, so the handlers are in time. Setting a property on a
    // fresh transaction object can't fail.
    let _ = js_sys::Reflect::set(&transaction, &BATCH_PROPERTY.into(), &batch.into());
    transaction.set_oncomplete(Some(self.on_complete.as_ref().unchecked_ref()));
    transaction.set_onabort(Some(self.on_abort.as_ref().unchecked_ref()));
  }

  fn transaction(&self) -> Result<(IdbTransaction, IdbObjectStore), WriteFailure> {
    if self.writes.borrow().is_closed {
      return Err(WriteFailure::Closed);
    }
    let transaction = self
      .database
      .transaction_with_str_and_mode(RECORDS, IdbTransactionMode::Readwrite)
      .map_err(|error| failure_of(&error))?;
    let records = transaction
      .object_store(RECORDS)
      .map_err(|error| failure_of(&error))?;
    Ok((transaction, records))
  }

  /// Queues every failed write again, with its latest value.
  fn retry_failed(&self) {
    let mut writes = self.writes.borrow_mut();
    let failed: Vec<String> = writes
      .open
      .iter()
      .filter(|(_, write)| write.state == WriteState::Failed)
      .map(|(key, _)| key.clone())
      .collect();
    for key in failed {
      if let Some(write) = writes.open.get_mut(&key) {
        write.state = WriteState::Queued;
      }
      writes.queued.push(key);
    }
  }

  /// The reason writes are failing, while any failed write is still open.
  fn failure(&self) -> Result<(), StoreError> {
    let writes = self.writes.borrow();
    let failed = writes
      .open
      .values()
      .filter(|write| write.state == WriteState::Failed)
      .count();
    if failed == 0 && !writes.is_closed {
      return Ok(());
    }
    Err(match writes.maybe_last_failure.unwrap_or(WriteFailure::Other) {
      WriteFailure::QuotaExceeded => StoreError::QuotaExceeded,
      WriteFailure::Closed => StoreError::Closed,
      WriteFailure::Other => StoreError::Database(format!("{failed} IndexedDB writes failed")),
    })
  }
}

impl Writes {
  /// Records the write as the record's latest and queues its key, once.
  fn queue(&mut self, key: String, maybe_value: Option<Vec<u8>>) {
    self.next_sequence = self.next_sequence.wrapping_add(1);
    self.queued_bytes = self
      .queued_bytes
      .saturating_add(maybe_value.as_ref().map_or(0, Vec::len));
    let write = OpenWrite {
      sequence: self.next_sequence,
      maybe_value,
      state: WriteState::Queued,
    };
    let was_queued = self
      .open
      .insert(key.clone(), write)
      .is_some_and(|earlier| earlier.state == WriteState::Queued);
    if !was_queued {
      self.queued.push(key);
    }
  }

  /// Only the latest write of a record closes it; an older one committing changes nothing.
  fn committed(&mut self, batch: u32) {
    for (key, sequence) in self.batches.remove(&batch).unwrap_or_default() {
      if self
        .open
        .get(&key)
        .is_some_and(|write| write.sequence == sequence && write.state == WriteState::Issued)
      {
        self.open.remove(&key);
      }
    }
  }

  fn aborted(&mut self, batch: u32, failure: WriteFailure) {
    for (key, sequence) in self.batches.remove(&batch).unwrap_or_default() {
      if let Some(write) = self.open.get_mut(&key)
        && write.sequence == sequence
        && write.state == WriteState::Issued
      {
        write.state = WriteState::Failed;
        self.maybe_last_failure = Some(failure);
      }
    }
  }

  /// A write of the batch that was aborted before it was sent (queued or already issued).
  fn fail_open(&mut self, key: &str, failure: WriteFailure) {
    if let Some(write) = self.open.get_mut(key)
      && write.state != WriteState::Failed
    {
      write.state = WriteState::Failed;
      self.maybe_last_failure = Some(failure);
    }
  }

  fn fail_queued(&mut self, key: &str, failure: WriteFailure) {
    if let Some(write) = self.open.get_mut(key)
      && write.state == WriteState::Queued
    {
      write.state = WriteState::Failed;
      self.maybe_last_failure = Some(failure);
    }
  }
}

impl Drop for IndexedDbRecords {
  fn drop(&mut self) {
    // Writes still queued are lost, like a page that is closed; owners flush before dropping.
    self.inner.database.set_onversionchange(None);
    self.inner.database.close();
  }
}

/// Issues the queued writes after the batch delay, unless the store is gone by then.
fn schedule(inner: Weak<Inner>) {
  wasm_bindgen_futures::spawn_local(async move {
    crate::platform::sleep(BATCH_DELAY).await;
    if let Some(inner) = inner.upgrade() {
      inner.issue_queued();
    }
  });
}

/// The batch a write transaction carries.
fn batch_of(event: &web_sys::Event) -> Option<u32> {
  let transaction = event.target()?;
  let batch = js_sys::Reflect::get(&transaction, &BATCH_PROPERTY.into())
    .ok()?
    .as_f64()?;
  // Written as a u32 above.
  Some(batch as u32)
}

fn failure_of(error: &JsValue) -> WriteFailure {
  error
    .dyn_ref::<web_sys::DomException>()
    .map_or(WriteFailure::Other, |error| classify(&error.name()))
}

fn classify(name: &str) -> WriteFailure {
  match name {
    QUOTA_EXCEEDED => WriteFailure::QuotaExceeded,
    "InvalidStateError" => WriteFailure::Closed,
    _ => WriteFailure::Other,
  }
}

async fn read_all(database: &IdbDatabase) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
  let transaction = database
    .transaction_with_str(RECORDS)
    .map_err(|error| idb::js_error("load", &error))?;
  let records = transaction
    .object_store(RECORDS)
    .map_err(|error| idb::js_error("load", &error))?;
  // Both lists come in key order, so they line up.
  let keys = records
    .get_all_keys()
    .map_err(|error| idb::js_error("load", &error))?;
  let values = records.get_all().map_err(|error| idb::js_error("load", &error))?;
  let keys: js_sys::Array = idb::request_result(&keys).await?.unchecked_into();
  let values: js_sys::Array = idb::request_result(&values).await?.unchecked_into();
  keys
    .iter()
    .zip(values.iter())
    .map(|(key, value)| {
      let id = key
        .as_string()
        .as_deref()
        .and_then(parse_id)
        .ok_or_else(|| StoreError::Corrupt("record key".into()))?;
      let sealed = value
        .dyn_ref::<js_sys::Uint8Array>()
        .ok_or_else(|| StoreError::Corrupt("record value".into()))?
        .to_vec();
      Ok((id, sealed))
    })
    .collect()
}
