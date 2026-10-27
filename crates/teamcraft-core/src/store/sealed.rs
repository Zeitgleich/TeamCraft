//! [`LocalStore`] over a plain record database, encrypted record by record (e2ee.md §9.1).
//!
//! For platforms without SQLCipher: the browser (IndexedDB) and Android (plain SQLite; the NDK has
//! no libcrypto for SQLCipher). Every record is sealed by `teamcraft-crypto::local_store` under a
//! pseudorandom lookup ID. Opening decrypts all records into a [`MemoryStore`], which answers every
//! read; writes update the cache and go to the backend right away. A backend that collects writes
//! may seal a record only when it writes it ([`RecordBackend::put_lazy`]), so a record rewritten
//! before then is sealed once (issue #326: the MLS state, then one record of megabytes, after
//! every group joined; one record per entry since #424).
//!
//! A group of writes that the backend rolls back stays in the cache: the running app goes on
//! with it (as with any failed write), the next start reads what the backend kept.

use std::sync::Arc;

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
#[cfg(any(test, target_arch = "wasm32"))]
use teamcraft_crypto::local_store::LOOKUP_ID_LEN;
use teamcraft_crypto::local_store::{LocalStoreKeys, LookupId};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{Base64Bytes, ClientMessageId, ConversationId, MessageId};

use super::{
  LocalStore, MediaRange, MemoryStore, MessageFilter, StoreError, StoredMessage, Table, WriteTally,
};

const ENTRY_NAMESPACE: &str = "entry";
const MESSAGE_NAMESPACE: &str = "message";

/// The encrypted store over a record backend; see the module docs.
pub struct SealedStore<B> {
  backend: B,
  /// Shared with the seals a backend runs later ([`LazySeal`]).
  keys: Arc<LocalStoreKeys>,
  cache: MemoryStore,
}

/// Seals one record when called (again, if a seal failed).
pub type LazySeal = Box<dyn Fn() -> Result<Vec<u8>, StoreError>>;

/// Where sealed records are kept: opaque IDs and opaque bytes.
pub trait RecordBackend {
  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError>;
  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError>;

  /// See [`LocalStore::purge_freed`]; backends without such files do nothing.
  fn purge_freed(&self) -> Result<(), StoreError> {
    Ok(())
  }

  /// See [`LocalStore::purge_freed_soon`].
  fn purge_freed_soon(&self) {}

  /// Puts the record `seal` makes, about `size_hint` bytes. A backend that collects writes may
  /// call `seal` only when it writes the record, and not at all when a newer write of the same
  /// record comes first. Writes keep their order either way.
  fn put_lazy(&self, id: &LookupId, size_hint: usize, seal: LazySeal) -> Result<(), StoreError> {
    let _ = size_hint;
    let sealed = seal()?;
    self.put_record(id, &sealed)
  }

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
  /// [`LocalStore::write_tally`] for the records. The cache keeps a group the backend rolled back,
  /// so the tally is the only way a caller learns that the disk lost it.
  fn write_tally(&self) -> Option<WriteTally> {
    None
  }
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
      keys: Arc::new(LocalStoreKeys::derive(database_key)),
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

  #[cfg(any(target_arch = "wasm32", all(test, feature = "sealed-sqlite")))]
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

  /// Seals an entry when the backend writes it. Until then the seal holds the value; it is wiped
  /// when the seal is dropped (sealed, or replaced by a newer write): entries hold the MLS state,
  /// with key package private keys and epoch secrets.
  fn write_entry(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    let id = self.entry_id(table, key);
    let pending = PendingEntry {
      table: table.name().to_string(),
      key: key.to_string(),
      value: zeroize::Zeroizing::new(value.to_vec()),
    };
    let size_hint = pending.size_hint();
    let keys = Arc::clone(&self.keys);
    let seal = Box::new(move || pending.seal(&keys, &id));
    self.backend.put_lazy(&id, size_hint, seal)
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
    self.write_entry(table, key, value)
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
    // Sealed at once: a message is written about once, so there is nothing to save by waiting.
    let id = self.message_id(message.conversation_id, message.client_message_id);
    let record = Record::Message {
      message: Box::new(message.clone()),
    };
    self
      .backend
      .put_record(&id, &seal_record(&self.keys, &id, &record)?)
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

  fn media_messages(
    &self,
    conversation: ConversationId,
    range: MediaRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    self.cache.media_messages(conversation, range, limit)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    for message in self.cache.messages(conversation, MessageFilter::All)? {
      self
        .backend
        .delete_record(&self.message_id(conversation, message.client_message_id))?;
    }
    self.cache.delete_messages(conversation)
  }

  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    self
      .backend
      .delete_record(&self.message_id(conversation, client_message_id))?;
    self.cache.delete_message(conversation, client_message_id)
  }

  fn purge_freed(&self) -> Result<(), StoreError> {
    self.backend.purge_freed()
  }

  fn purge_freed_soon(&self) {
    self.backend.purge_freed_soon();
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

  fn write_tally(&self) -> Option<WriteTally> {
    self.backend.write_tally()
  }
}

/// An entry waiting to be sealed ([`SealedStore::write_entry`]); the value is wiped on drop.
struct PendingEntry {
  table: String,
  key: String,
  value: zeroize::Zeroizing<Vec<u8>>,
}

impl PendingEntry {
  /// About the sealed size (the value travels as base64), for a backend's batch accounting.
  fn size_hint(&self) -> usize {
    self.value.len().saturating_mul(4) / 3 + self.key.len() + 64
  }

  fn seal(&self, keys: &LocalStoreKeys, id: &LookupId) -> Result<Vec<u8>, StoreError> {
    let record = Record::Entry {
      table: self.table.clone(),
      key: self.key.clone(),
      value: Base64Bytes::new(self.value.to_vec()),
    };
    let sealed = seal_record(keys, id, &record);
    // The record's copy of the value doesn't wipe itself.
    if let Record::Entry { value, .. } = record {
      zeroize::Zeroize::zeroize(&mut value.into_vec());
    }
    sealed
  }
}

impl<B> std::fmt::Debug for SealedStore<B> {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("SealedStore").finish_non_exhaustive()
  }
}

fn seal_record(keys: &LocalStoreKeys, id: &LookupId, record: &Record) -> Result<Vec<u8>, StoreError> {
  let plaintext = zeroize::Zeroizing::new(
    serde_json::to_vec(record).map_err(|error| StoreError::Corrupt(error.to_string()))?,
  );
  keys
    .seal(id, &plaintext, &mut OsRng)
    .map_err(|error| StoreError::Database(error.to_string()))
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
