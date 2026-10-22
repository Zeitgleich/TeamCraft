//! [`LocalStore`] on IndexedDB, encrypted record by record (e2ee.md §9).
//!
//! [`LocalStore`] is synchronous, IndexedDB is not. Opening reads and decrypts every record into a
//! [`MemoryStore`], which then answers all reads. Each write updates the cache and issues the
//! IndexedDB request right away in its own transaction; IndexedDB runs read-write transactions on
//! the same object store in the order they were created, so the database follows the cache in
//! order. [`IndexedDbStore::flush`] waits until everything written so far is committed.

use std::cell::Cell;
use std::rc::Rc;

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::local_store::{LocalStoreKeys, LookupId};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{Base64Bytes, ClientMessageId, ConversationId};
use wasm_bindgen::prelude::Closure;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{IdbDatabase, IdbTransactionMode};

use super::idb::{self, RECORDS};
use crate::store::{LocalStore, MemoryStore, MessageFilter, StoreError, StoredMessage, Table};

const ENTRY_NAMESPACE: &str = "entry";
const MESSAGE_NAMESPACE: &str = "message";

/// The encrypted browser store; see the module docs.
pub struct IndexedDbStore {
  database: IdbDatabase,
  keys: LocalStoreKeys,
  cache: MemoryStore,
  failed_writes: Rc<Cell<u32>>,
  on_write_error: Closure<dyn FnMut(web_sys::Event)>,
}

/// What a sealed record holds: the plaintext name next to the value, so loading can rebuild the
/// cache from pseudorandom storage keys.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
  Entry {
    table: String,
    key: String,
    value: Base64Bytes,
  },
  Message {
    message: StoredMessage,
  },
}

impl IndexedDbStore {
  /// Opens the database `name` and decrypts it into memory with `database_key`.
  pub async fn open(name: &str, database_key: &SymmetricKey) -> Result<Self, StoreError> {
    let database = idb::open_database(name).await?;
    let failed_writes = Rc::new(Cell::new(0u32));
    let on_write_error = Closure::<dyn FnMut(web_sys::Event)>::new({
      let failed_writes = failed_writes.clone();
      move |_| {
        failed_writes.set(failed_writes.get().saturating_add(1));
        tracing::error!("an IndexedDB write failed");
      }
    });
    let store = Self {
      database,
      keys: LocalStoreKeys::derive(database_key),
      cache: MemoryStore::new(),
      failed_writes,
      on_write_error,
    };
    store.load().await?;
    Ok(store)
  }

  /// Waits until every write issued so far is committed; fails if any write failed since the last
  /// flush.
  pub async fn flush(&self) -> Result<(), StoreError> {
    // A read-only transaction over the same store starts only after earlier read-write ones ended.
    let transaction = self
      .database
      .transaction_with_str(RECORDS)
      .map_err(|error| idb::js_error("flush", &error))?;
    idb::transaction_done(&transaction).await?;
    match self.failed_writes.replace(0) {
      0 => Ok(()),
      failed => Err(StoreError::Database(format!("{failed} IndexedDB writes failed"))),
    }
  }

  async fn load(&self) -> Result<(), StoreError> {
    let transaction = self
      .database
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
    for (key, value) in keys.iter().zip(values.iter()) {
      self.load_record(&key, &value)?;
    }
    Ok(())
  }

  fn load_record(&self, key: &JsValue, value: &JsValue) -> Result<(), StoreError> {
    let id = key
      .as_string()
      .as_deref()
      .and_then(parse_id)
      .ok_or_else(|| StoreError::Corrupt("record key".into()))?;
    let sealed = value
      .dyn_ref::<js_sys::Uint8Array>()
      .ok_or_else(|| StoreError::Corrupt("record value".into()))?
      .to_vec();
    let plaintext = self.keys.open(&id, &sealed).map_err(|_| StoreError::WrongKey)?;
    let record: Record = serde_json::from_slice(plaintext.expose_secret())
      .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    match record {
      Record::Entry { table, key, value } => match Table::from_name(&table) {
        Some(table) => self.cache.put(table, &key, value.as_slice()),
        // Written by a newer version; kept in IndexedDB, ignored here.
        None => {
          tracing::warn!(table = %table, "skipping a record of an unknown table");
          Ok(())
        },
      },
      Record::Message { message } => self.cache.upsert_message(&message),
    }
  }

  fn write(&self, id: &LookupId, record: &Record) -> Result<(), StoreError> {
    let plaintext = zeroize::Zeroizing::new(
      serde_json::to_vec(record).map_err(|error| StoreError::Corrupt(error.to_string()))?,
    );
    let sealed = self
      .keys
      .seal(id, &plaintext, &mut OsRng)
      .map_err(|error| StoreError::Database(error.to_string()))?;
    let value = js_sys::Uint8Array::from(sealed.as_slice());
    let records = self.begin_write()?;
    records
      .put_with_key(&value, &JsValue::from_str(&format_id(id)))
      .map_err(|error| idb::js_error("put", &error))?;
    Ok(())
  }

  fn erase(&self, id: &LookupId) -> Result<(), StoreError> {
    self
      .begin_write()?
      .delete(&JsValue::from_str(&format_id(id)))
      .map_err(|error| idb::js_error("delete", &error))?;
    Ok(())
  }

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

  fn entry_id(&self, table: Table, key: &str) -> LookupId {
    let mut name = table.name().as_bytes().to_vec();
    name.push(0);
    name.extend_from_slice(key.as_bytes());
    self.keys.lookup_id(ENTRY_NAMESPACE, &name)
  }

  fn message_id(&self, conversation: ConversationId, client_message_id: ClientMessageId) -> LookupId {
    let mut name = conversation.to_string().into_bytes();
    name.push(0);
    name.extend_from_slice(client_message_id.as_bytes());
    self.keys.lookup_id(MESSAGE_NAMESPACE, &name)
  }
}

impl LocalStore for IndexedDbStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    self.cache.get(table, key)
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    self.cache.put(table, key, value)?;
    self.write(
      &self.entry_id(table, key),
      &Record::Entry {
        table: table.name().to_string(),
        key: key.to_string(),
        value: Base64Bytes::new(value.to_vec()),
      },
    )
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.cache.delete(table, key)?;
    self.erase(&self.entry_id(table, key))
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.cache.list(table)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    self.cache.upsert_message(message)?;
    self.write(
      &self.message_id(message.conversation_id, message.client_message_id),
      &Record::Message {
        message: message.clone(),
      },
    )
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.cache.message(conversation, client_message_id)
  }

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.cache.messages(conversation, filter)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    for message in self.cache.messages(conversation, MessageFilter::All)? {
      self.erase(&self.message_id(conversation, message.client_message_id))?;
    }
    self.cache.delete_messages(conversation)
  }
}

impl std::fmt::Debug for IndexedDbStore {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("IndexedDbStore").finish_non_exhaustive()
  }
}

impl Drop for IndexedDbStore {
  fn drop(&mut self) {
    self.database.close();
  }
}

fn format_id(id: &LookupId) -> String {
  id.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_id(text: &str) -> Option<LookupId> {
  if text.len() != 64 {
    return None;
  }
  let mut id = [0u8; 32];
  for (index, byte) in id.iter_mut().enumerate() {
    *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
  }
  Some(LookupId(id))
}
