//! [`LocalStore`] on IndexedDB: a [`SealedStore`] whose records live in one object store
//! (e2ee.md §9.1).
//!
//! [`LocalStore`] is synchronous, IndexedDB is not. Opening reads every record; each write issues
//! its IndexedDB request right away in its own transaction. IndexedDB runs read-write transactions
//! on the same object store in the order they were created, so the database follows the cache in
//! order. [`IndexedDbStore::flush`] waits until everything written so far is committed.
//!
//! A write that fails (quota exceeded, the database closed for a newer version) is kept and written
//! again on the next flush, so the database catches up with the cache once space is free again
//! instead of silently rolling back on the next reload (e2ee.md §9.4). The flush reports the
//! failure; the app shows it to the user.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbTransaction, IdbTransactionMode};

use super::idb::{self, RECORDS};
use crate::store::StoreError;
use crate::store::sealed::{RecordBackend, SealedStore, format_id, parse_id};

/// Property on a write transaction naming the record it writes, read back by the shared handlers.
const RECORD_PROPERTY: &str = "teamcraftRecord";
/// Property on a write transaction with the write's sequence number.
const SEQUENCE_PROPERTY: &str = "teamcraftSequence";
/// `DOMException.name` of a write refused for lack of space.
const QUOTA_EXCEEDED: &str = "QuotaExceededError";

/// The encrypted browser store.
pub type IndexedDbStore = SealedStore<IndexedDbRecords>;

/// The IndexedDB object store holding the sealed records.
pub struct IndexedDbRecords {
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
  maybe_last_failure: Option<WriteFailure>,
  /// Another page (a newer version) asked for the database; it was closed, writes wait for a reload.
  is_closed: bool,
}

/// The latest write of one record. Its value is kept until it committed, to write it again.
struct OpenWrite {
  sequence: u32,
  /// `None` deletes the record.
  maybe_value: Option<Vec<u8>>,
  has_failed: bool,
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

  /// Waits until every write issued so far is committed. Writes that failed are issued again, and
  /// the flush fails with the reason, until they went through.
  pub async fn flush(&self) -> Result<(), StoreError> {
    let backend = self.backend();
    backend.retry_failed();
    if !backend.writes.borrow().is_closed {
      // A read-only transaction over the same store starts only after earlier read-write ones ended.
      let transaction = backend
        .database
        .transaction_with_str(RECORDS)
        .map_err(|error| idb::js_error("flush", &error))?;
      idb::transaction_done(&transaction).await?;
    }
    backend.failure()
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
}

impl IndexedDbRecords {
  fn new(database: IdbDatabase) -> Self {
    let writes = Rc::new(RefCell::new(Writes::default()));
    let on_complete = Closure::<dyn FnMut(web_sys::Event)>::new({
      let writes = writes.clone();
      move |event: web_sys::Event| {
        if let Some((key, sequence)) = written_record(&event) {
          writes.borrow_mut().committed(&key, sequence);
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
        if let Some((key, sequence)) = written_record(&event) {
          writes.borrow_mut().failed(&key, sequence, failure);
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
      database,
      writes,
      on_complete,
      on_abort,
      _on_version_change: on_version_change,
    }
  }

  /// Remembers the write and issues it; a write that can't even start counts as failed.
  fn write(&self, key: String, maybe_value: Option<Vec<u8>>) {
    let sequence = self.writes.borrow_mut().open(key.clone(), maybe_value.clone());
    if let Err(failure) = self.issue(&key, sequence, maybe_value.as_deref()) {
      self.writes.borrow_mut().failed(&key, sequence, failure);
    }
  }

  fn issue(&self, key: &str, sequence: u32, maybe_value: Option<&[u8]>) -> Result<(), WriteFailure> {
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
    let record = JsValue::from_str(key);
    match maybe_value {
      Some(value) => records.put_with_key(&js_sys::Uint8Array::from(value), &record),
      None => records.delete(&record),
    }
    .map_err(|error| failure_of(&error))?;
    // Only a transaction that carries the write reports on it: one whose request was refused (a full
    // disk can refuse at once) commits empty, and must not close the failed write. The events fire
    // after this returns, so the handlers are in time.
    // Setting a property on a fresh transaction object can't fail.
    let _ = js_sys::Reflect::set(&transaction, &RECORD_PROPERTY.into(), &key.into());
    let _ = js_sys::Reflect::set(&transaction, &SEQUENCE_PROPERTY.into(), &sequence.into());
    transaction.set_oncomplete(Some(self.on_complete.as_ref().unchecked_ref()));
    transaction.set_onabort(Some(self.on_abort.as_ref().unchecked_ref()));
    Ok(())
  }

  /// Issues every failed write again, with its latest value.
  fn retry_failed(&self) {
    let failed: Vec<(String, Option<Vec<u8>>)> = self
      .writes
      .borrow()
      .open
      .iter()
      .filter(|(_, write)| write.has_failed)
      .map(|(key, write)| (key.clone(), write.maybe_value.clone()))
      .collect();
    for (key, maybe_value) in failed {
      self.write(key, maybe_value);
    }
  }

  /// The reason writes are failing, while any failed write is still open.
  fn failure(&self) -> Result<(), StoreError> {
    let writes = self.writes.borrow();
    let failed = writes.open.values().filter(|write| write.has_failed).count();
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
  fn open(&mut self, key: String, maybe_value: Option<Vec<u8>>) -> u32 {
    self.next_sequence = self.next_sequence.wrapping_add(1);
    let sequence = self.next_sequence;
    self.open.insert(
      key,
      OpenWrite {
        sequence,
        maybe_value,
        has_failed: false,
      },
    );
    sequence
  }

  /// Only the latest write of a record closes it; an older one committing changes nothing.
  fn committed(&mut self, key: &str, sequence: u32) {
    if self.open.get(key).is_some_and(|write| write.sequence == sequence) {
      self.open.remove(key);
    }
  }

  fn failed(&mut self, key: &str, sequence: u32, failure: WriteFailure) {
    if let Some(write) = self.open.get_mut(key)
      && write.sequence == sequence
    {
      write.has_failed = true;
      self.maybe_last_failure = Some(failure);
    }
  }
}

impl Drop for IndexedDbRecords {
  fn drop(&mut self) {
    self.database.set_onversionchange(None);
    self.database.close();
  }
}

/// The record key and sequence number a write transaction carries.
fn written_record(event: &web_sys::Event) -> Option<(String, u32)> {
  let transaction = event.target()?;
  let key = js_sys::Reflect::get(&transaction, &RECORD_PROPERTY.into())
    .ok()?
    .as_string()?;
  let sequence = js_sys::Reflect::get(&transaction, &SEQUENCE_PROPERTY.into())
    .ok()?
    .as_f64()?;
  // Written as a u32 above.
  Some((key, sequence as u32))
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
