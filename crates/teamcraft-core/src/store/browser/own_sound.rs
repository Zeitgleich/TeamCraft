//! The user's own message sound in the browser (notifications.md, "Message sounds"; #800): the
//! picked file, with its name, in a database of its own. It stays on this device: it is never
//! uploaded or synced, so the server never sees it. The page decodes it with Web Audio when it
//! first plays; the browser keeps a stored `File` in its own blob storage on disk.

use wasm_bindgen::{JsCast, JsValue};
use web_sys::{Blob, IdbDatabase, IdbTransactionMode};

use super::idb::{RECORDS, js_error, open_database, request_result, transaction_done};
use crate::store::StoreError;

const DATABASE: &str = "teamcraft-sounds";
/// The one entry: the file picked for new messages.
const OWN_MESSAGE_SOUND: &str = "own-message-sound";

/// Keeps `file` as the own message sound, replacing the one before.
pub async fn own_sound_put(file: &Blob) -> Result<(), StoreError> {
  let database = open_database(DATABASE).await?;
  let transaction = write(&database)?;
  transaction
    .object_store(RECORDS)
    .map_err(|error| js_error("store", &error))?
    .put_with_key(file, &JsValue::from_str(OWN_MESSAGE_SOUND))
    .map_err(|error| js_error("put", &error))?;
  transaction_done(&transaction).await
}

/// The kept file (a `File`, so it has its name), `None` when none was picked.
pub async fn own_sound_get() -> Result<Option<Blob>, StoreError> {
  let database = open_database(DATABASE).await?;
  let transaction = database
    .transaction_with_str(RECORDS)
    .map_err(|error| js_error("transaction", &error))?;
  let request = transaction
    .object_store(RECORDS)
    .map_err(|error| js_error("store", &error))?
    .get(&JsValue::from_str(OWN_MESSAGE_SOUND))
    .map_err(|error| js_error("get", &error))?;
  Ok(request_result(&request).await?.dyn_into::<Blob>().ok())
}

pub async fn own_sound_delete() -> Result<(), StoreError> {
  let database = open_database(DATABASE).await?;
  let transaction = write(&database)?;
  transaction
    .object_store(RECORDS)
    .map_err(|error| js_error("store", &error))?
    .delete(&JsValue::from_str(OWN_MESSAGE_SOUND))
    .map_err(|error| js_error("delete", &error))?;
  transaction_done(&transaction).await
}

fn write(database: &IdbDatabase) -> Result<web_sys::IdbTransaction, StoreError> {
  database
    .transaction_with_str_and_mode(RECORDS, IdbTransactionMode::Readwrite)
    .map_err(|error| js_error("transaction", &error))
}
