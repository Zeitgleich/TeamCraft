//! [`LocalStore`] on IndexedDB: a [`SealedStore`] whose records live in one object store
//! (e2ee.md §9.1).
//!
//! [`LocalStore`] is synchronous, IndexedDB is not. Opening reads every record's ciphertext into
//! memory, which the store decrypts only when a record is asked for (issue #155, #778); writes are queued
//! and issued together in one transaction a moment later ([`BATCH_DELAY`], or at once when much is
//! queued or on [`IndexedDbStore::flush`]). Of several writes of one record in that time only the
//! last is issued: a catch-up rewrites some records after every group it processes, and one
//! transaction per write left the browser writing for minutes after the catch-up was done
//! (docs/performance.md). Records are sealed only when their batch is issued, so a replaced write
//! costs no encryption either, and a batch holding a large record waits [`LARGE_BATCH_EXTRA`]
//! longer to take the next rewrite of it along (issue #326: the MLS state was one record of
//! megabytes, sealed after every group a catch-up joined; since #424 it is one record per entry).
//! IndexedDB runs read-write transactions on the same object store in the order they were created,
//! so the database follows the cache in order.
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
use std::collections::BTreeMap;
use std::rc::{Rc, Weak};
use std::time::Duration;

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbObjectStore, IdbTransaction, IdbTransactionMode};

use super::idb::{self, OpenMode, RECORDS};
use crate::store::sealed::{LazySeal, RecordBackend, SealedStore, format_id, parse_id};
use crate::store::write_queue::{Issue, OpenValue, RecordValue, WriteFailure, Writes};
use crate::store::{SealedStoreLimits, StoreError};

/// How long writes are collected before they are issued together. Short enough that little is
/// lost when the page dies, long enough that a burst of writes becomes one transaction.
const BATCH_DELAY: Duration = Duration::from_millis(50);
/// How much longer a batch over `LARGE_BATCH_BYTES` waits after its delay.
const LARGE_BATCH_EXTRA: Duration = Duration::from_millis(200);
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
  /// Every record's sealed bytes as of its latest issued write (what is queued is in `writes`).
  records: RefCell<BTreeMap<LookupId, Vec<u8>>>,
  writes: Rc<RefCell<Writes>>,
  on_complete: Closure<dyn FnMut(web_sys::Event)>,
  on_abort: Closure<dyn FnMut(web_sys::Event)>,
  _on_version_change: Closure<dyn FnMut(web_sys::Event)>,
}

impl SealedStore<IndexedDbRecords> {
  /// Opens the database `name` with `database_key`, reading its ciphertext into memory; a page that
  /// owns the store (it holds the Web Lock) upgrades it to this version and builds its catalog.
  pub async fn open(name: &str, database_key: &SymmetricKey) -> Result<Self, StoreError> {
    let backend = IndexedDbRecords::read(name, OpenMode::Upgrade).await?;
    Self::open_backend(backend, database_key, SealedStoreLimits::default())
  }

  /// [`Self::open`] for the service worker, a second writer beside the page that owns the store: at
  /// whatever version the database has, never upgrading it ([`OpenMode::AsIs`]) or building its
  /// catalog.
  pub async fn open_without_upgrade(name: &str, database_key: &SymmetricKey) -> Result<Self, StoreError> {
    let backend = IndexedDbRecords::read(name, OpenMode::AsIs).await?;
    Self::open_backend_without_building(backend, database_key, SealedStoreLimits::default())
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
    inner.writes.borrow_mut().retry_failed();
    inner.issue_queued();
    if !inner.writes.borrow().is_closed {
      // A read-only transaction over the same store starts only after earlier read-write ones ended.
      let transaction = inner
        .database
        .transaction_with_str(RECORDS)
        .map_err(|error| idb::js_error("flush", &error))?;
      idb::transaction_done(&transaction).await?;
    }
    inner.writes.borrow_mut().failure()
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
    self.write(
      format_id(id),
      Some(RecordValue::Sealed(sealed.to_vec())),
      sealed.len(),
    );
    Ok(())
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    self.write(format_id(id), None, 0);
    Ok(())
  }

  fn put_lazy(&self, id: &LookupId, size_hint: usize, seal: LazySeal) -> Result<(), StoreError> {
    self.write(format_id(id), Some(RecordValue::Unsealed(seal)), size_hint);
    Ok(())
  }

  fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
    match self.inner.writes.borrow_mut().latest(&format_id(id))? {
      Some(OpenValue::Put(sealed)) => return Ok(Some(sealed)),
      Some(OpenValue::Delete) => return Ok(None),
      None => {},
    }
    Ok(self.inner.records.borrow().get(id).cloned())
  }

  fn records_after(
    &self,
    maybe_after: Option<&LookupId>,
    limit: usize,
  ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
    let records = self.inner.records.borrow();
    let range = match maybe_after {
      Some(after) => records.range((std::ops::Bound::Excluded(*after), std::ops::Bound::Unbounded)),
      None => records.range(..),
    };
    let mut writes = self.inner.writes.borrow_mut();
    let mut batch = Vec::with_capacity(limit.min(1024));
    for (id, sealed) in range {
      match writes.latest(&format_id(id))? {
        Some(OpenValue::Put(latest)) => batch.push((*id, latest)),
        Some(OpenValue::Delete) => {},
        None => batch.push((*id, sealed.clone())),
      }
      if batch.len() == limit {
        break;
      }
    }
    Ok(batch)
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
      writes.group_depth == 0 && writes.is_over_limit()
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
  /// Opens the database `name` as `mode` says and reads every record's ciphertext.
  async fn read(name: &str, mode: OpenMode) -> Result<Self, StoreError> {
    let database = idb::open_database_with(name, mode).await?;
    match read_all(&database).await {
      Ok(records) => Ok(Self::new(database, records.into_iter().collect())),
      Err(error) => {
        // Closed, so a reset can delete the database (issue #160) instead of being blocked by it.
        database.close();
        Err(error)
      },
    }
  }

  fn new(database: IdbDatabase, records: BTreeMap<LookupId, Vec<u8>>) -> Self {
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
        writes.borrow_mut().close();
      }
    });
    database.set_onversionchange(Some(on_version_change.as_ref().unchecked_ref()));
    Self {
      inner: Rc::new(Inner {
        database,
        records: RefCell::new(records),
        writes,
        on_complete,
        on_abort,
        _on_version_change: on_version_change,
      }),
    }
  }

  /// Queues the write; the batch goes out after [`BATCH_DELAY`], or now when much is queued.
  fn write(&self, key: String, maybe_value: Option<RecordValue>, bytes: usize) {
    let is_due = {
      let mut writes = self.inner.writes.borrow_mut();
      if writes.queue(key, maybe_value, bytes) {
        true
      } else {
        if !writes.is_scheduled {
          writes.is_scheduled = true;
          schedule(Rc::downgrade(&self.inner), BATCH_DELAY, true);
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
    let keys = {
      let mut writes = self.writes.borrow_mut();
      if writes.has_queued() {
        writes.retry_failed();
      }
      writes.take_queued()
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
      let record = JsValue::from_str(key);
      let maybe_id = parse_id(key);
      let (issued, sequence) = match writes.issue(key) {
        Some(Issue::Put { sealed, sequence }) => {
          if let Some(id) = maybe_id {
            self.records.borrow_mut().insert(id, sealed.to_vec());
          }
          (
            records.put_with_key(&js_sys::Uint8Array::from(sealed), &record),
            sequence,
          )
        },
        Some(Issue::Delete { sequence }) => {
          if let Some(id) = maybe_id {
            self.records.borrow_mut().remove(&id);
          }
          (records.delete(&record), sequence)
        },
        // Replaced, or dropped for a failed seal.
        None => continue,
      };
      match issued {
        Ok(_) => {
          writes.issued(key);
          carried.push((key.clone(), sequence));
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
    let batch = writes.start_batch(carried);
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
}

impl Drop for IndexedDbRecords {
  fn drop(&mut self) {
    // Writes still queued are lost, like a page that is closed; owners flush before dropping.
    self.inner.database.set_onversionchange(None);
    self.inner.database.close();
  }
}

/// Issues the queued writes after `delay`, unless the store is gone by then. A large batch gets
/// [`LARGE_BATCH_EXTRA`] once when `may_extend`.
fn schedule(inner: Weak<Inner>, delay: Duration, may_extend: bool) {
  wasm_bindgen_futures::spawn_local(async move {
    crate::platform::sleep(delay).await;
    let Some(inner) = inner.upgrade() else {
      return;
    };
    let is_large = inner.writes.borrow().is_large();
    if may_extend && is_large {
      schedule(Rc::downgrade(&inner), LARGE_BATCH_EXTRA, false);
    } else {
      inner.issue_queued();
    }
  });
}

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
