//! [`LocalStore`] on IndexedDB: a [`SealedStore`] whose records live in one object store
//! (e2ee.md §9.1).
//!
//! [`LocalStore`] is synchronous, IndexedDB is not. Opening reads every record; each write issues
//! its IndexedDB request right away in its own transaction. IndexedDB runs read-write transactions
//! on the same object store in the order they were created, so the database follows the cache in
//! order. [`IndexedDbStore::flush`] waits until everything written so far is committed.

use std::cell::Cell;
use std::rc::Rc;

use teamcraft_crypto::local_store::LookupId;
use teamcraft_crypto::secret::SymmetricKey;
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbTransactionMode};

use super::idb::{self, RECORDS};
use crate::store::StoreError;
use crate::store::sealed::{RecordBackend, SealedStore, format_id, parse_id};

/// The encrypted browser store.
pub type IndexedDbStore = SealedStore<IndexedDbRecords>;

/// The IndexedDB object store holding the sealed records.
pub struct IndexedDbRecords {
  database: IdbDatabase,
  failed_writes: Rc<Cell<u32>>,
  on_write_error: Closure<dyn FnMut(web_sys::Event)>,
}

impl SealedStore<IndexedDbRecords> {
  /// Opens the database `name` and decrypts it into memory with `database_key`.
  pub async fn open(name: &str, database_key: &SymmetricKey) -> Result<Self, StoreError> {
    let database = idb::open_database(name).await?;
    let records = read_all(&database).await?;
    let failed_writes = Rc::new(Cell::new(0u32));
    let on_write_error = Closure::<dyn FnMut(web_sys::Event)>::new({
      let failed_writes = failed_writes.clone();
      move |_| {
        failed_writes.set(failed_writes.get().saturating_add(1));
        tracing::error!("an IndexedDB write failed");
      }
    });
    let backend = IndexedDbRecords {
      database,
      failed_writes,
      on_write_error,
    };
    Self::load(backend, database_key, records)
  }

  /// Waits until every write issued so far is committed; fails if any write failed since the last
  /// flush.
  pub async fn flush(&self) -> Result<(), StoreError> {
    let backend = self.backend();
    // A read-only transaction over the same store starts only after earlier read-write ones ended.
    let transaction = backend
      .database
      .transaction_with_str(RECORDS)
      .map_err(|error| idb::js_error("flush", &error))?;
    idb::transaction_done(&transaction).await?;
    match backend.failed_writes.replace(0) {
      0 => Ok(()),
      failed => Err(StoreError::Database(format!("{failed} IndexedDB writes failed"))),
    }
  }
}

impl RecordBackend for IndexedDbRecords {
  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    let value = js_sys::Uint8Array::from(sealed);
    self
      .begin_write()?
      .put_with_key(&value, &JsValue::from_str(&format_id(id)))
      .map_err(|error| idb::js_error("put", &error))?;
    Ok(())
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    self
      .begin_write()?
      .delete(&JsValue::from_str(&format_id(id)))
      .map_err(|error| idb::js_error("delete", &error))?;
    Ok(())
  }
}

impl IndexedDbRecords {
  /// A fresh read-write transaction; it commits on its own once its request finished.
  fn begin_write(&self) -> Result<web_sys::IdbObjectStore, StoreError> {
    let transaction = self
      .database
      .transaction_with_str_and_mode(RECORDS, IdbTransactionMode::Readwrite)
      .map_err(|error| idb::js_error("write", &error))?;
    transaction.set_onabort(Some(self.on_write_error.as_ref().unchecked_ref()));
    transaction
      .object_store(RECORDS)
      .map_err(|error| idb::js_error("write", &error))
  }
}

impl Drop for IndexedDbRecords {
  fn drop(&mut self) {
    self.database.close();
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
