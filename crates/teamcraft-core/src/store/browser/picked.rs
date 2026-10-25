//! The browser's copies of picked files that wait for their upload (see `crate::picked_files`), in
//! a database of their own: a reload continues their upload. The browser keeps a stored `File` in
//! its own blob storage on disk, not in memory.

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Blob, IdbDatabase, IdbTransactionMode};

use super::idb::{RECORDS, js_error, open_database, request_result, transaction_done};
use crate::store::StoreError;

const DATABASE: &str = "teamcraft-picked-files";

pub(crate) async fn put(handle: &str, blob: &Blob) -> Result<(), StoreError> {
  let database = open_database(DATABASE).await?;
  let transaction = write(&database)?;
  let store = transaction
    .object_store(RECORDS)
    .map_err(|error| js_error("store", &error))?;
  store
    .put_with_key(blob, &JsValue::from_str(handle))
    .map_err(|error| js_error("put", &error))?;
  transaction_done(&transaction).await
}

pub(crate) async fn get(handle: &str) -> Result<Option<Blob>, StoreError> {
  let database = open_database(DATABASE).await?;
  let transaction = database
    .transaction_with_str(RECORDS)
    .map_err(|error| js_error("transaction", &error))?;
  let request = transaction
    .object_store(RECORDS)
    .map_err(|error| js_error("store", &error))?
    .get(&JsValue::from_str(handle))
    .map_err(|error| js_error("get", &error))?;
  Ok(request_result(&request).await?.dyn_into::<Blob>().ok())
}

pub(crate) async fn delete(handle: &str) -> Result<(), StoreError> {
  let database = open_database(DATABASE).await?;
  let transaction = write(&database)?;
  transaction
    .object_store(RECORDS)
    .map_err(|error| js_error("store", &error))?
    .delete(&JsValue::from_str(handle))
    .map_err(|error| js_error("delete", &error))?;
  transaction_done(&transaction).await
}

/// The handles of every stored copy.
pub(crate) async fn handles() -> Result<Vec<String>, StoreError> {
  let database = open_database(DATABASE).await?;
  let transaction = database
    .transaction_with_str(RECORDS)
    .map_err(|error| js_error("transaction", &error))?;
  let request = transaction
    .object_store(RECORDS)
    .map_err(|error| js_error("store", &error))?
    .get_all_keys()
    .map_err(|error| js_error("keys", &error))?;
  let keys: js_sys::Array = request_result(&request).await?.unchecked_into();
  Ok(keys.iter().filter_map(|key| key.as_string()).collect())
}

fn write(database: &IdbDatabase) -> Result<web_sys::IdbTransaction, StoreError> {
  database
    .transaction_with_str_and_mode(RECORDS, IdbTransactionMode::Readwrite)
    .map_err(|error| js_error("transaction", &error))
}
