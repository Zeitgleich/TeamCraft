//! Small secrets kept across page loads, outside any account's store: the web sign-in's tokens.
//!
//! Each value is sealed with AES-GCM under a non-extractable Web Crypto key, stored in the same
//! IndexedDB database, as [`super::browser_key`] does for the database key. It protects against a
//! copy of the browser's files, not against script running on the origin (e2ee.md §9.1).

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{CryptoKey, IdbTransactionMode};
use zeroize::Zeroizing;

use super::browser_key::{self, WRAPPING_KEY};
use super::idb::{self, KEYS, RECORDS};
use crate::store::StoreError;

/// Seals `plaintext` under `slot` of the database `name`, replacing what was there.
pub async fn vault_put(name: &str, slot: &str, plaintext: &[u8]) -> Result<(), StoreError> {
  let database = idb::open_database(name).await?;
  let result = put(&database, slot, plaintext).await;
  database.close();
  result
}

/// The value under `slot`, or `None` if there is none or it can no longer be opened (the key was
/// replaced, or the record changed); such a record is removed.
pub async fn vault_get(name: &str, slot: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
  let database = idb::open_database(name).await?;
  let result = get(&database, slot).await;
  database.close();
  result
}

/// Removes the value under `slot`.
pub async fn vault_delete(name: &str, slot: &str) -> Result<(), StoreError> {
  let database = idb::open_database(name).await?;
  let result = delete(&database, slot).await;
  database.close();
  result
}

async fn put(database: &web_sys::IdbDatabase, slot: &str, plaintext: &[u8]) -> Result<(), StoreError> {
  let subtle = browser_key::subtle()?;
  let key = match stored_key(database).await? {
    Some(key) => key,
    None => {
      let key = browser_key::generate_wrapping_key(&subtle).await?;
      write(database, KEYS, WRAPPING_KEY, &key).await?;
      key
    },
  };
  let sealed = browser_key::encrypt(&subtle, &key, plaintext).await?;
  write(
    database,
    RECORDS,
    slot,
    &js_sys::Uint8Array::from(sealed.as_slice()),
  )
  .await
}

async fn get(database: &web_sys::IdbDatabase, slot: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
  let Some(sealed) = read(database, RECORDS, slot).await? else {
    return Ok(None);
  };
  let maybe_plaintext = match (
    stored_key(database).await?,
    sealed.dyn_into::<js_sys::Uint8Array>(),
  ) {
    (Some(key), Ok(sealed)) => browser_key::decrypt(&browser_key::subtle()?, &key, &sealed.to_vec()).await?,
    _ => None,
  };
  if maybe_plaintext.is_none() {
    tracing::warn!("a sealed browser value could not be opened; removing it");
    delete(database, slot).await?;
  }
  Ok(maybe_plaintext)
}

async fn stored_key(database: &web_sys::IdbDatabase) -> Result<Option<CryptoKey>, StoreError> {
  Ok(
    read(database, KEYS, WRAPPING_KEY)
      .await?
      .and_then(|key| key.dyn_into().ok()),
  )
}

async fn read(
  database: &web_sys::IdbDatabase,
  store: &str,
  key: &str,
) -> Result<Option<JsValue>, StoreError> {
  let transaction = database
    .transaction_with_str(store)
    .map_err(|error| idb::js_error("read vault", &error))?;
  let request = transaction
    .object_store(store)
    .and_then(|records| records.get(&JsValue::from_str(key)))
    .map_err(|error| idb::js_error("read vault", &error))?;
  let value = idb::request_result(&request).await?;
  Ok((!value.is_undefined()).then_some(value))
}

async fn write(
  database: &web_sys::IdbDatabase,
  store: &str,
  key: &str,
  value: &JsValue,
) -> Result<(), StoreError> {
  let transaction = database
    .transaction_with_str_and_mode(store, IdbTransactionMode::Readwrite)
    .map_err(|error| idb::js_error("write vault", &error))?;
  // Structured clone keeps a CryptoKey non-extractable in storage.
  transaction
    .object_store(store)
    .and_then(|records| records.put_with_key(value, &JsValue::from_str(key)))
    .map_err(|error| idb::js_error("write vault", &error))?;
  idb::transaction_done(&transaction).await
}

async fn delete(database: &web_sys::IdbDatabase, slot: &str) -> Result<(), StoreError> {
  let transaction = database
    .transaction_with_str_and_mode(RECORDS, IdbTransactionMode::Readwrite)
    .map_err(|error| idb::js_error("delete vault", &error))?;
  transaction
    .object_store(RECORDS)
    .and_then(|records| records.delete(&JsValue::from_str(slot)))
    .map_err(|error| idb::js_error("delete vault", &error))?;
  idb::transaction_done(&transaction).await
}
