//! [`LocalStore`] over a plain record database, encrypted record by record (e2ee.md §9.1).
//!
//! For platforms without SQLCipher: the browser (IndexedDB) and Android (plain SQLite; the NDK has
//! no libcrypto for SQLCipher). Every record is sealed by `teamcraft-crypto::local_store` under a
//! pseudorandom lookup ID. Opening decrypts all records into a [`MemoryStore`], which answers every
//! read; writes update the cache and go to the backend right away. A group of writes that the
//! backend rolls back stays in the cache: the running app goes on with it (as with any failed
//! write), the next start reads what the backend kept.

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
#[cfg(any(test, target_arch = "wasm32"))]
use teamcraft_crypto::local_store::LOOKUP_ID_LEN;
use teamcraft_crypto::local_store::{LocalStoreKeys, LookupId};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{Base64Bytes, ClientMessageId, ConversationId, MessageId};

use super::{LocalStore, MemoryStore, MessageFilter, StoreError, StoredMessage, Table};

const ENTRY_NAMESPACE: &str = "entry";
const MESSAGE_NAMESPACE: &str = "message";

/// The encrypted store over a record backend; see the module docs.
pub struct SealedStore<B> {
  backend: B,
  keys: LocalStoreKeys,
  cache: MemoryStore,
}

/// Where sealed records are kept: opaque IDs and opaque bytes.
pub trait RecordBackend {
  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError>;
  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError>;
  /// [`LocalStore::begin_writes`] for the records.
  fn begin_writes(&self) -> Result<(), StoreError> {
    Ok(())
  }
  /// [`LocalStore::end_writes`] for the records.
  fn end_writes(&self) -> Result<(), StoreError> {
    Ok(())
  }
  /// [`LocalStore::write_soon`] for the records.
  fn write_soon(&self) {}
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
    message: Box<StoredMessage>,
  },
}

// Constructed by the IndexedDB and SQLite backends, which not every build has.
#[cfg_attr(
  not(any(feature = "sealed-sqlite", target_arch = "wasm32", test)),
  allow(dead_code)
)]
impl<B: RecordBackend> SealedStore<B> {
  /// Decrypts `records` (everything the backend holds) into the cache. If no record opens, the key
  /// is wrong; if only some don't, the database is damaged.
  // Only the IndexedDB (wasm) and sealed-SQLite (Android) backends open a sealed store.
  #[cfg_attr(not(any(target_arch = "wasm32", feature = "sealed-sqlite")), allow(dead_code))]
  pub(crate) fn load(
    backend: B,
    database_key: &SymmetricKey,
    records: impl IntoIterator<Item = (LookupId, Vec<u8>)>,
  ) -> Result<Self, StoreError> {
    let store = Self {
      backend,
      keys: LocalStoreKeys::derive(database_key),
      cache: MemoryStore::new(),
    };
    let (mut opened, mut maybe_failure) = (0usize, None);
    for (id, sealed) in records {
      match store.load_record(&id, &sealed) {
        Ok(()) => opened += 1,
        Err(error) => {
          maybe_failure.get_or_insert(error);
        },
      }
    }
    match maybe_failure {
      None => Ok(store),
      Some(StoreError::WrongKey) if opened > 0 => Err(StoreError::Corrupt(
        "some records of the local database don't open".into(),
      )),
      Some(error) => Err(error),
    }
  }

  #[cfg(target_arch = "wasm32")]
  pub(crate) fn backend(&self) -> &B {
    &self.backend
  }

  // Only the IndexedDB (wasm) and sealed-SQLite (Android) backends open a sealed store.
  #[cfg_attr(not(any(target_arch = "wasm32", feature = "sealed-sqlite")), allow(dead_code))]
  fn load_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    let plaintext = self.keys.open(id, sealed).map_err(|_| StoreError::WrongKey)?;
    let record: Record = serde_json::from_slice(plaintext.expose_secret())
      .map_err(|error| StoreError::Corrupt(error.to_string()))?;
    match record {
      Record::Entry { table, key, value } => match Table::from_name(&table) {
        Some(table) => self.cache.put(table, &key, value.as_slice()),
        // Written by a newer version; kept in the backend, ignored here.
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
    self.backend.put_record(id, &sealed)
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

impl<B: RecordBackend> LocalStore for SealedStore<B> {
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
    self.backend.delete_record(&self.entry_id(table, key))
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.cache.list(table)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    self.cache.upsert_message(message)?;
    self.write(
      &self.message_id(message.conversation_id, message.client_message_id),
      &Record::Message {
        message: Box::new(message.clone()),
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

  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.cache.message_by_server_id(conversation, server_id)
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
      self
        .backend
        .delete_record(&self.message_id(conversation, message.client_message_id))?;
    }
    self.cache.delete_messages(conversation)
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    self.backend.begin_writes()
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    self.backend.end_writes()
  }

  fn write_soon(&self) {
    self.backend.write_soon();
  }
}

impl<B> std::fmt::Debug for SealedStore<B> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SealedStore").finish_non_exhaustive()
  }
}

/// Lowercase hex, for backends with text keys.
#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) fn format_id(id: &LookupId) -> String {
  id.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) fn parse_id(text: &str) -> Option<LookupId> {
  if text.len() != LOOKUP_ID_LEN * 2 {
    return None;
  }
  let mut id = [0u8; LOOKUP_ID_LEN];
  for (index, byte) in id.iter_mut().enumerate() {
    *byte = u8::from_str_radix(text.get(index * 2..index * 2 + 2)?, 16).ok()?;
  }
  Some(LookupId(id))
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeMap;
  use std::sync::Mutex;

  use super::*;
  use crate::store::tests::exercise_store;

  /// A backend that keeps the sealed bytes, so tests can reopen and inspect them.
  #[derive(Default)]
  struct Records(Mutex<BTreeMap<LookupId, Vec<u8>>>);

  impl RecordBackend for &Records {
    fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
      self.0.lock().unwrap().insert(*id, sealed.to_vec());
      Ok(())
    }

    fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
      self.0.lock().unwrap().remove(id);
      Ok(())
    }
  }

  #[test]
  fn sealed_store_behaves_like_a_store_and_reopens() {
    let records = Records::default();
    let key = SymmetricKey::generate(&mut OsRng);
    let store = SealedStore::load(&records, &key, []).unwrap();
    exercise_store(&store);
    store.put(Table::Users, "9", b"recognizable plaintext").unwrap();
    let snapshot: Vec<(LookupId, Vec<u8>)> = records.0.lock().unwrap().clone().into_iter().collect();
    assert!(
      snapshot
        .iter()
        .all(|(_, sealed)| !sealed.windows(12).any(|window| window == b"recognizable")),
      "nothing readable reaches the backend"
    );

    let reopened = SealedStore::load(&records, &key, snapshot.clone()).unwrap();
    assert_eq!(
      reopened.get(Table::Users, "9").unwrap(),
      Some(b"recognizable plaintext".to_vec())
    );
    let wrong = SealedStore::load(&records, &SymmetricKey::generate(&mut OsRng), snapshot);
    assert!(matches!(wrong, Err(StoreError::WrongKey)));
  }

  #[test]
  fn ids_round_trip_through_hex() {
    let id = LookupId([0xab; LOOKUP_ID_LEN]);
    assert_eq!(parse_id(&format_id(&id)), Some(id));
    assert_eq!(parse_id("zz"), None);
  }
}
