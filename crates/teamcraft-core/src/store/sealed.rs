//! [`LocalStore`] over a plain record database, encrypted record by record (e2ee.md §9.1).
//!
//! For platforms without SQLCipher: the browser (IndexedDB) and the mobile apps (plain SQLite; the
//! Android NDK has no libcrypto for SQLCipher). Every record is sealed by
//! `teamcraft-crypto::local_store` under a pseudorandom lookup ID.
//!
//! Records are read when they are asked for, not when the store opens (issue #155): an entry by its
//! lookup ID, a table's entries through its key directory, a conversation's messages through its
//! message index. Both are kept in the store's catalog ([`super::sealed_catalog`]), sealed like
//! every other record. What was read last stays decrypted in bounded caches
//! ([`SealedStoreLimits`]); a page of messages costs that page, not the conversation, and opening
//! costs nothing that grows with the history. A store written before the catalog existed builds it
//! once when it is opened, in batches, and goes on where it stopped when that is interrupted.
//!
//! Writes go to the backend right away and update the caches. A backend that collects writes may
//! seal a record only when it writes it ([`RecordBackend::put_lazy`]), so a record rewritten before
//! then is sealed once (issue #326). The catalog nodes a group of writes changed are sealed once,
//! when the group ends; every write outside a group is a group of its own, so a message and its
//! place in the index reach the disk together.
//!
//! When the disk may no longer hold what the caches do (a group of writes rolled back, or another
//! process committed: the iOS notification extension) the caches are dropped and read again. The
//! entries a rolled-back group deleted are remembered ([`LocalStore::lost_deletes`]) for a backend
//! that kept them ([`RecordBackend::write_tally`] `None`, the browser's): a caller that writes its
//! state again (the MLS state) deletes them again.

use std::collections::{BTreeSet, HashMap};
use std::ops::Bound;
use std::sync::{Arc, Mutex, MutexGuard};

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::local_store::LOOKUP_ID_LEN;
use teamcraft_crypto::local_store::{LocalStoreKeys, LookupId};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{Base64Bytes, ClientMessageId, ConversationId, MessageId};
use zeroize::Zeroizing;

use super::lru::Lru;
use super::sealed_catalog::{
  CATALOG_TABLE, CatalogState, META_KEY, MessageEntry, MessageKey, NodeCache, NodeChange, ScopedNodes,
  decode, encode, messages_scope,
};
use super::sealed_index::{self, Direction};
use super::{
  LocalStore, MediaRange, MessageEdge, MessageFilter, MessageSlot, StoreError, StoredMessage, Table,
  WriteTally,
};

const ENTRY_NAMESPACE: &str = "entry";
const MESSAGE_NAMESPACE: &str = "message";
/// Records decrypted per batch while an old store's catalog is built: one group of writes each.
const CATALOG_BATCH: usize = 512;

/// The encrypted store over a record backend; see the module docs.
pub struct SealedStore<B> {
  backend: B,
  /// Shared with the seals a backend runs later ([`LazySeal`]).
  keys: Arc<LocalStoreKeys>,
  state: Mutex<State>,
  entries: Mutex<EntryLog>,
}

/// How much a [`SealedStore`] keeps decrypted. Each cache drops what was used least recently
/// first; nothing else grows with the store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealedStoreLimits {
  /// Decrypted entry values (MLS state, members, settings), in bytes.
  pub max_entry_bytes: usize,
  /// Decoded messages.
  pub max_messages: usize,
  /// Index roots and index pages, each (a page holds up to 256 entries of a few dozen bytes).
  pub max_index_nodes: usize,
}

/// The caches, the catalog's nodes and the open group, under one lock.
struct State {
  /// Entry values by table and key; `None` caches that there is none.
  values: Lru<(Table, String), Option<Zeroizing<Vec<u8>>>>,
  messages: Lru<(ConversationId, [u8; 16]), StoredMessage>,
  message_nodes: NodeCache<MessageKey, MessageEntry>,
  directory_nodes: NodeCache<String, ()>,
  /// Writes per conversation since the caches were last dropped ([`LocalStore::message_revision`]).
  revisions: HashMap<ConversationId, u64>,
  /// Raised whenever the caches are dropped: part of every revision.
  epoch: u64,
  depth: u32,
  /// The backend's tally when last looked at: a change means the disk moved under the caches.
  maybe_tally: Option<WriteTally>,
}

/// The entries the open group of writes touched, and deletes the backend rolled back.
#[derive(Default)]
struct EntryLog {
  depth: u32,
  /// Entries written (`false`) or deleted (`true`) in the open group.
  in_group: Vec<(Table, String, bool)>,
  /// Entries deleted in groups the backend rolled back ([`LocalStore::lost_deletes`]).
  lost_deletes: BTreeSet<(Table, String)>,
}

/// Seals one record when called (again, if a seal failed).
pub type LazySeal = Box<dyn Fn() -> Result<Vec<u8>, StoreError>>;

/// Where sealed records are kept: opaque IDs and opaque bytes.
pub trait RecordBackend {
  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError>;
  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError>;
  /// The record's latest bytes, including a write not on disk yet.
  fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError>;
  /// Up to `limit` records after `maybe_after` in ID order: an old store's catalog is built by
  /// walking them once.
  fn records_after(
    &self,
    maybe_after: Option<&LookupId>,
    limit: usize,
  ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError>;

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
  /// Makes the open group fail when it ends, if the backend can undo it: a step of the group failed
  /// without a write failing (a record that didn't decode).
  fn fail_group(&self) {}
  /// [`LocalStore::write_soon`] for the records.
  fn write_soon(&self) {}
  /// [`LocalStore::write_tally`] for the records. `None`: the backend keeps every write it took
  /// and has no other writer.
  fn write_tally(&self) -> Option<WriteTally> {
    None
  }
}

/// What a sealed record holds: the plaintext name next to the value, so a record names what it is
/// (the catalog of an old store is built from them).
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

/// Where a step of the catalog build left off ([`SealedStore::build_catalog`]).
#[derive(Debug, PartialEq, Eq)]
enum BuildProgress {
  Complete,
  /// Stopped after the batches it was allowed (tests interrupt a build this way).
  Interrupted,
}

impl Default for SealedStoreLimits {
  /// About 16 MiB at most: enough for the MLS state and the open conversations of a large
  /// workspace (docs/performance.md, "Local store").
  fn default() -> Self {
    Self {
      max_entry_bytes: 8 * 1024 * 1024,
      max_messages: 4_096,
      max_index_nodes: 256,
    }
  }
}

impl SealedStoreLimits {
  /// For a process that reads a few records and exits under a tight memory limit: the iOS
  /// notification extension (about 24 MB for everything).
  pub fn minimal() -> Self {
    Self {
      max_entry_bytes: 512 * 1024,
      max_messages: 64,
      max_index_nodes: 32,
    }
  }
}

// Constructed by the IndexedDB and SQLite backends, which not every build has.
#[cfg_attr(
  not(any(feature = "sealed-sqlite", target_arch = "wasm32", test)),
  allow(dead_code)
)]
impl<B: RecordBackend> SealedStore<B> {
  /// Opens the store over `backend` without reading its records. A store without a complete
  /// catalog builds it first ([`Self::build_catalog`]). A key that opens no record is
  /// [`StoreError::WrongKey`]; a record that doesn't open with a key that opens others is
  /// [`StoreError::Corrupt`], when it is read.
  pub(crate) fn open_backend(
    backend: B,
    database_key: &SymmetricKey,
    limits: SealedStoreLimits,
  ) -> Result<Self, StoreError> {
    let store = Self::unbuilt(backend, database_key, limits)?;
    store.build_catalog(None)?;
    Ok(store)
  }

  /// Opens the store over a database an older version still owns (the service worker beside an
  /// older page, which writes without keeping the catalog): the catalog is not built, so what isn't
  /// in it yet is not listed (a message the preview doesn't find is fetched), and nothing marks it
  /// complete for the version that takes the database over and builds it.
  #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
  pub(crate) fn open_without_catalog(
    backend: B,
    database_key: &SymmetricKey,
    limits: SealedStoreLimits,
  ) -> Result<Self, StoreError> {
    Self::unbuilt(backend, database_key, limits)
  }

  /// The store before its catalog is checked: tests interrupt the build.
  fn unbuilt(backend: B, database_key: &SymmetricKey, limits: SealedStoreLimits) -> Result<Self, StoreError> {
    let maybe_tally = backend.write_tally();
    Ok(Self {
      backend,
      keys: Arc::new(LocalStoreKeys::derive(database_key)),
      state: Mutex::new(State {
        values: Lru::new(limits.max_entry_bytes),
        messages: Lru::new(limits.max_messages),
        message_nodes: NodeCache::new(true, limits.max_index_nodes),
        directory_nodes: NodeCache::new(false, limits.max_index_nodes),
        revisions: HashMap::new(),
        epoch: 0,
        depth: 0,
        maybe_tally,
      }),
      entries: Mutex::default(),
    })
  }

  #[cfg(any(target_arch = "wasm32", all(test, feature = "sealed-sqlite")))]
  pub(crate) fn backend(&self) -> &B {
    &self.backend
  }

  /// Builds the catalog of a store written before it existed (issue #155): every record is
  /// decrypted once and noted in its table's directory or its conversation's message index, a
  /// batch per group of writes with the progress saved in the same group, so a build that is
  /// killed goes on after its last batch. At most `maybe_batches` batches (tests).
  fn build_catalog(&self, maybe_batches: Option<usize>) -> Result<BuildProgress, StoreError> {
    let mut maybe_after = match self.read_catalog_state()? {
      Some(CatalogState::Complete) => return Ok(BuildProgress::Complete),
      Some(CatalogState::Building { after }) => {
        Some(parse_id(&after).ok_or_else(|| StoreError::Corrupt("catalog progress".into()))?)
      },
      None => None,
    };
    let is_resumed = maybe_after.is_some();
    let (mut opened, mut batches) = (0usize, 0usize);
    loop {
      if maybe_batches.is_some_and(|allowed| batches == allowed) {
        return Ok(BuildProgress::Interrupted);
      }
      let batch = self.backend.records_after(maybe_after.as_ref(), CATALOG_BATCH)?;
      let Some((last, _)) = batch.last() else {
        break;
      };
      let last = *last;
      self.grouped(|state| {
        let mut maybe_failure = None;
        for (id, sealed) in &batch {
          match self.open_record(id, sealed) {
            Ok(record) => {
              opened += 1;
              self.note_in_catalog(state, record)?;
            },
            Err(error) => {
              maybe_failure.get_or_insert(error);
            },
          }
        }
        match maybe_failure {
          None => {},
          // No record opened: the key is wrong (issue #160 handles a store whose every record is
          // damaged the same way).
          Some(_) if opened == 0 && !is_resumed => return Err(StoreError::WrongKey),
          Some(error) => return Err(error),
        }
        let progress = CatalogState::Building {
          after: format_id(&last),
        };
        self.write_named(CATALOG_TABLE, META_KEY, &encode(&progress)?)
      })?;
      batches += 1;
      maybe_after = Some(last);
    }
    self.grouped(|_| self.write_named(CATALOG_TABLE, META_KEY, &encode(&CatalogState::Complete)?))?;
    if batches > 0 {
      tracing::info!(batches, records = opened, "built the local store's catalog");
    }
    Ok(BuildProgress::Complete)
  }

  /// How far the catalog is built. A damaged state builds it again: the build only adds what is
  /// missing. (Under another key the state has another lookup ID and is simply not there.)
  fn read_catalog_state(&self) -> Result<Option<CatalogState>, StoreError> {
    let decoded = self
      .read_named(CATALOG_TABLE, META_KEY)
      .and_then(|maybe_bytes| maybe_bytes.map(|bytes| decode(&bytes)).transpose());
    match decoded {
      Err(StoreError::Corrupt(reason)) => {
        tracing::warn!(%reason, "the local store's catalog state is damaged; building it again");
        Ok(None)
      },
      other => other,
    }
  }

  /// Notes a record of an old store in the catalog. The catalog's own records (of a build that was
  /// interrupted) and entries of tables this version doesn't know are left out.
  fn note_in_catalog(&self, state: &mut State, record: Record) -> Result<(), StoreError> {
    match record {
      Record::Entry { table, key, .. } => match Table::from_name(&table) {
        Some(table) => self.directory(state, table, |nodes| sealed_index::upsert(nodes, key, ())),
        None if table == CATALOG_TABLE => Ok(()),
        None => {
          tracing::warn!(table = %table, "skipping a record of an unknown table");
          Ok(())
        },
      },
      Record::Message { message } => self.index(state, message.conversation_id, |nodes| {
        sealed_index::upsert(nodes, MessageKey::of(&message), MessageEntry::of(&message))
      }),
    }
  }

  /// Runs `write` as a group of writes ([`LocalStore::begin_writes`]); a failed step fails the
  /// group where the backend can undo it.
  fn grouped<T>(&self, write: impl FnOnce(&mut State) -> Result<T, StoreError>) -> Result<T, StoreError> {
    self.begin_writes()?;
    let result = write(&mut self.lock());
    if result.is_err() {
      self.backend.fail_group();
    }
    let ended = self.end_writes();
    let value = result?;
    ended?;
    Ok(value)
  }

  /// The state, after dropping the caches if the disk moved under them since the last look
  /// (another process committed, or a group of writes rolled back). Inside a group nobody else
  /// commits, and the group's own changes must stay.
  fn fresh(&self) -> MutexGuard<'_, State> {
    let mut state = self.lock();
    if state.depth == 0 {
      let maybe_tally = self.backend.write_tally();
      if maybe_tally != state.maybe_tally {
        state.forget_all();
        state.maybe_tally = maybe_tally;
      }
    }
    state
  }

  fn lock(&self) -> MutexGuard<'_, State> {
    // A poisoned lock only means another thread panicked; the caches stay consistent with what
    // each step wrote.
    self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Runs `step` on `table`'s key directory.
  fn directory<T>(
    &self,
    state: &mut State,
    table: Table,
    step: impl FnOnce(&mut ScopedNodes<'_, String, ()>) -> Result<T, StoreError>,
  ) -> Result<T, StoreError> {
    let read = |key: &str| {
      self
        .read_named(CATALOG_TABLE, key)
        .map(|value| value.map(|value| value.to_vec()))
    };
    step(&mut ScopedNodes {
      cache: &mut state.directory_nodes,
      scope: table.name(),
      read: &read,
    })
  }

  /// Runs `step` on `conversation`'s message index.
  fn index<T>(
    &self,
    state: &mut State,
    conversation: ConversationId,
    step: impl FnOnce(&mut ScopedNodes<'_, MessageKey, MessageEntry>) -> Result<T, StoreError>,
  ) -> Result<T, StoreError> {
    let read = |key: &str| {
      self
        .read_named(CATALOG_TABLE, key)
        .map(|value| value.map(|value| value.to_vec()))
    };
    let scope = messages_scope(conversation);
    step(&mut ScopedNodes {
      cache: &mut state.message_nodes,
      scope: &scope,
      read: &read,
    })
  }

  /// The keys of `conversation`'s index that `keep` picks, walking from `from` in `direction`
  /// until `keep` says to stop (`None`).
  fn index_keys(
    &self,
    state: &mut State,
    conversation: ConversationId,
    from: Bound<&MessageKey>,
    direction: Direction,
    keep: &mut dyn FnMut(&MessageKey, MessageEntry) -> Option<bool>,
  ) -> Result<Vec<MessageKey>, StoreError> {
    let mut keys = Vec::new();
    self.index(state, conversation, |nodes| {
      sealed_index::scan(
        nodes,
        from,
        direction,
        &mut |key, entry| match keep(key, *entry) {
          Some(true) => {
            keys.push(*key);
            true
          },
          Some(false) => true,
          None => false,
        },
      )
    })?;
    Ok(keys)
  }

  /// The messages of `keys` in that order; one the index lists but the disk lacks is left out.
  fn load_messages(
    &self,
    state: &mut State,
    conversation: ConversationId,
    keys: &[MessageKey],
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let mut messages = Vec::with_capacity(keys.len());
    for key in keys {
      if let Some(message) = self.cached_message(state, conversation, key.client_message_id())? {
        messages.push(message);
      }
    }
    Ok(messages)
  }

  fn cached_message(
    &self,
    state: &mut State,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    let key = (conversation, *client_message_id.as_bytes());
    if let Some(message) = state.messages.get(&key) {
      return Ok(Some(message.clone()));
    }
    let id = self.message_id(conversation, client_message_id);
    let Some(sealed) = self.backend.get_record(&id)? else {
      return Ok(None);
    };
    let Record::Message { message } = self.open_record(&id, &sealed)? else {
      return Err(StoreError::Corrupt("a message record holds an entry".into()));
    };
    state.messages.insert(key, (*message).clone(), 1);
    Ok(Some(*message))
  }

  fn cached_value(&self, state: &mut State, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    let cache_key = (table, key.to_string());
    if let Some(maybe_value) = state.values.get(&cache_key) {
      return Ok(maybe_value.as_ref().map(|value| value.to_vec()));
    }
    let maybe_value = self.read_named(table.name(), key)?;
    let result = maybe_value.as_ref().map(|value| value.to_vec());
    let weight = entry_weight(key, maybe_value.as_ref().map_or(0, |value| value.len()));
    state.values.insert(cache_key, maybe_value, weight);
    Ok(result)
  }

  /// The value of an entry, read from the backend.
  fn read_named(&self, table: &str, key: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
    let id = self.entry_id(table, key);
    let Some(sealed) = self.backend.get_record(&id)? else {
      return Ok(None);
    };
    match self.open_record(&id, &sealed)? {
      Record::Entry {
        table: found_table,
        key: found_key,
        value,
      } if found_table == table && found_key == key => Ok(Some(Zeroizing::new(value.into_vec()))),
      _ => Err(StoreError::Corrupt(
        "a record holds something else than its name".into(),
      )),
    }
  }

  fn open_record(&self, id: &LookupId, sealed: &[u8]) -> Result<Record, StoreError> {
    // The key opened the catalog, so a record that doesn't open is damaged (or swapped).
    let plaintext = self
      .keys
      .open(id, sealed)
      .map_err(|_| StoreError::Corrupt("a record of the local database doesn't open".into()))?;
    serde_json::from_slice(plaintext.expose_secret()).map_err(|error| StoreError::Corrupt(error.to_string()))
  }

  /// Seals an entry when the backend writes it. Until then the seal holds the value; it is wiped
  /// when the seal is dropped (sealed, or replaced by a newer write): entries hold the MLS state,
  /// with key package private keys and epoch secrets.
  fn write_named(&self, table: &str, key: &str, value: &[u8]) -> Result<(), StoreError> {
    let id = self.entry_id(table, key);
    let pending = PendingEntry {
      table: table.to_string(),
      key: key.to_string(),
      value: Zeroizing::new(value.to_vec()),
    };
    let size_hint = pending.size_hint();
    let keys = Arc::clone(&self.keys);
    let seal = Box::new(move || pending.seal(&keys, &id));
    self.backend.put_lazy(&id, size_hint, seal)
  }

  /// Writes the catalog nodes the ending group changed.
  fn flush_catalog(&self, state: &mut State) -> Result<(), StoreError> {
    let changes = state
      .message_nodes
      .take_dirty()?
      .into_iter()
      .chain(state.directory_nodes.take_dirty()?);
    for NodeChange { key, maybe_value } in changes {
      match maybe_value {
        Some(value) => self.write_named(CATALOG_TABLE, &key, &value)?,
        None => self.backend.delete_record(&self.entry_id(CATALOG_TABLE, &key))?,
      }
    }
    Ok(())
  }

  pub(crate) fn entry_id(&self, table: &str, key: &str) -> LookupId {
    let mut name = table.as_bytes().to_vec();
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

  /// Removes a stored message from the disk, its index and the cache.
  fn remove_message(&self, state: &mut State, message: &StoredMessage) -> Result<(), StoreError> {
    let conversation = message.conversation_id;
    self
      .backend
      .delete_record(&self.message_id(conversation, message.client_message_id))?;
    self.index(state, conversation, |nodes| {
      sealed_index::remove(nodes, &MessageKey::of(message)).map(|_| ())
    })?;
    state
      .messages
      .remove(&(conversation, *message.client_message_id.as_bytes()));
    state.bump(conversation);
    Ok(())
  }
}

impl<B> SealedStore<B> {
  fn entry_log(&self) -> MutexGuard<'_, EntryLog> {
    // A poisoned lock only means another thread panicked; the log stays usable.
    self
      .entries
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Notes a write (`is_delete`: a delete) of an entry that the backend took: inside a group it
  /// counts once the group ended, outside one it reached the disk.
  fn note_entry_write(&self, table: Table, key: &str, is_delete: bool) {
    let mut log = self.entry_log();
    if log.depth > 0 {
      log.in_group.push((table, key.to_string(), is_delete));
    } else {
      log.lost_deletes.remove(&(table, key.to_string()));
    }
  }
}

impl State {
  /// The disk may no longer hold what the caches do: nothing kept or derived counts any more.
  fn forget_all(&mut self) {
    self.values.clear();
    self.messages.clear();
    self.message_nodes.clear();
    self.directory_nodes.clear();
    self.revisions.clear();
    self.epoch += 1;
  }

  fn bump(&mut self, conversation: ConversationId) {
    let writes = self.revisions.entry(conversation).or_default();
    *writes = writes.wrapping_add(1);
  }
}

impl<B: RecordBackend> LocalStore for SealedStore<B> {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    self.cached_value(&mut self.fresh(), table, key)
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    self.grouped(|state| {
      self.write_named(table.name(), key, value)?;
      self.directory(state, table, |nodes| {
        sealed_index::upsert(nodes, key.to_string(), ())
      })?;
      let weight = entry_weight(key, value.len());
      state.values.insert(
        (table, key.to_string()),
        Some(Zeroizing::new(value.to_vec())),
        weight,
      );
      Ok(())
    })?;
    self.note_entry_write(table, key, false);
    Ok(())
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    let deleted = self.grouped(|state| {
      state
        .values
        .insert((table, key.to_string()), None, entry_weight(key, 0));
      self.backend.delete_record(&self.entry_id(table.name(), key))?;
      self.directory(state, table, |nodes| {
        sealed_index::remove(nodes, &key.to_string()).map(|_| ())
      })
    });
    match deleted {
      Ok(()) => {
        self.note_entry_write(table, key, true);
        Ok(())
      },
      Err(error) => {
        // Maybe gone from the cache, maybe not from the disk (inside a group the group fails too).
        let mut log = self.entry_log();
        if log.depth > 0 {
          log.in_group.push((table, key.to_string(), true));
        } else {
          log.lost_deletes.insert((table, key.to_string()));
        }
        Err(error)
      },
    }
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.list_from(table, "")
  }

  fn list_prefix(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    self.list_from(table, prefix)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    let conversation = message.conversation_id;
    self.grouped(|state| {
      // A message that moved in the order (sent, so now at its server ID) leaves its old place.
      if let Some(earlier) = self.cached_message(state, conversation, message.client_message_id)?
        && MessageKey::of(&earlier) != MessageKey::of(message)
      {
        self.index(state, conversation, |nodes| {
          sealed_index::remove(nodes, &MessageKey::of(&earlier)).map(|_| ())
        })?;
      }
      // Sealed at once: a message is written about once, so there is nothing to save by waiting.
      let id = self.message_id(conversation, message.client_message_id);
      let record = Record::Message {
        message: Box::new(message.clone()),
      };
      self
        .backend
        .put_record(&id, &seal_record(&self.keys, &id, &record)?)?;
      self.index(state, conversation, |nodes| {
        sealed_index::upsert(nodes, MessageKey::of(message), MessageEntry::of(message))
      })?;
      state.messages.insert(
        (conversation, *message.client_message_id.as_bytes()),
        message.clone(),
        1,
      );
      state.bump(conversation);
      Ok(())
    })
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    self.cached_message(&mut self.fresh(), conversation, client_message_id)
  }

  /// One place of the index: server messages sort by their ID.
  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    let mut state = self.fresh();
    let keys = self.index_keys(
      &mut state,
      conversation,
      Bound::Included(&MessageKey::first_at(server_id)),
      Direction::Forward,
      &mut |key, entry| {
        key
          .is_at(server_id)
          .then_some(entry.server_id() == Some(server_id))
      },
    )?;
    Ok(
      self
        .load_messages(&mut state, conversation, &keys)?
        .into_iter()
        .next(),
    )
  }

  /// Walks the index from that end; lines, pending messages and placeholders don't count.
  fn edge_server_id(
    &self,
    conversation: ConversationId,
    edge: MessageEdge,
  ) -> Result<Option<MessageId>, StoreError> {
    let direction = match edge {
      MessageEdge::Oldest => Direction::Forward,
      MessageEdge::Newest => Direction::Backward,
    };
    let mut maybe_found = None;
    self.index_keys(
      &mut self.fresh(),
      conversation,
      Bound::Unbounded,
      direction,
      &mut |_, entry| match entry.readable_server_id() {
        Some(id) => {
          maybe_found = Some(id);
          None
        },
        None => Some(false),
      },
    )?;
    Ok(maybe_found)
  }

  fn messages_at(
    &self,
    conversation: ConversationId,
    slot: MessageSlot,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let mut state = self.fresh();
    let keys = match slot {
      MessageSlot::At(position) => self.index_keys(
        &mut state,
        conversation,
        Bound::Included(&MessageKey::first_at(position)),
        Direction::Forward,
        &mut |key, _| key.is_at(position).then_some(true),
      )?,
      MessageSlot::Pending => self.index_keys(
        &mut state,
        conversation,
        Bound::Included(&MessageKey::first_pending()),
        Direction::Forward,
        &mut |_, _| Some(true),
      )?,
    };
    self.load_messages(&mut state, conversation, &keys)
  }

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let mut state = self.fresh();
    let keys = self.index_keys(
      &mut state,
      conversation,
      Bound::Unbounded,
      Direction::Forward,
      &mut |_, entry| Some(entry.may_match(filter)),
    )?;
    let mut messages = self.load_messages(&mut state, conversation, &keys)?;
    messages.retain(|message| message.matches(filter));
    Ok(messages)
  }

  fn message_revision(&self, conversation: ConversationId) -> Option<u64> {
    let state = self.fresh();
    let writes = state.revisions.get(&conversation).copied().unwrap_or(0);
    Some((state.epoch << 32) | (writes & u64::from(u32::MAX)))
  }

  /// Walks the index from the range's start, reading only the messages with media it returns.
  fn media_messages(
    &self,
    conversation: ConversationId,
    range: MediaRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let (from, direction) = match range {
      MediaRange::Older(Some(from)) => (Bound::Excluded(MessageKey::first_at(from)), Direction::Backward),
      MediaRange::Older(None) => (Bound::Unbounded, Direction::Backward),
      MediaRange::Newer(from) => (Bound::Excluded(MessageKey::last_at(from)), Direction::Forward),
    };
    let mut state = self.fresh();
    let mut found = 0;
    let keys = self.index_keys(
      &mut state,
      conversation,
      from.as_ref(),
      direction,
      &mut |_, entry| {
        if found == limit {
          return None;
        }
        let is_media = entry.has_media();
        found += usize::from(is_media);
        Some(is_media)
      },
    )?;
    let mut messages = self.load_messages(&mut state, conversation, &keys)?;
    messages.retain(StoredMessage::has_media);
    Ok(messages)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    self.grouped(|state| {
      let keys = self.index_keys(
        state,
        conversation,
        Bound::Unbounded,
        Direction::Forward,
        &mut |_, _| Some(true),
      )?;
      for key in keys {
        self
          .backend
          .delete_record(&self.message_id(conversation, key.client_message_id()))?;
      }
      self.index(state, conversation, |nodes| sealed_index::clear(nodes))?;
      state.messages.retain(|(held, _)| *held != conversation);
      state.bump(conversation);
      Ok(())
    })
  }

  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    self.grouped(
      |state| match self.cached_message(state, conversation, client_message_id)? {
        Some(message) => self.remove_message(state, &message),
        None => self
          .backend
          .delete_record(&self.message_id(conversation, client_message_id)),
      },
    )
  }

  fn purge_freed(&self) -> Result<(), StoreError> {
    self.backend.purge_freed()
  }

  fn purge_freed_soon(&self) {
    self.backend.purge_freed_soon();
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    // Looked at before the group starts: once it is open nobody else commits.
    let mut state = self.fresh();
    self.backend.begin_writes()?;
    state.depth += 1;
    drop(state);
    self.entry_log().depth += 1;
    Ok(())
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    let mut state = self.lock();
    state.depth = state.depth.saturating_sub(1);
    if state.depth == 0
      && (state.message_nodes.has_dirty() || state.directory_nodes.has_dirty())
      && let Err(error) = self.flush_catalog(&mut state)
    {
      tracing::error!(%error, "could not write the local store's catalog");
      self.backend.fail_group();
    }
    let ended = self.backend.end_writes();
    if state.depth == 0 {
      if ended.is_err() {
        state.forget_all();
      }
      state.maybe_tally = self.backend.write_tally();
    }
    drop(state);
    let mut log = self.entry_log();
    log.depth = log.depth.saturating_sub(1);
    if log.depth == 0 {
      let touched = std::mem::take(&mut log.in_group);
      for (table, key, is_delete) in touched {
        if ended.is_ok() {
          log.lost_deletes.remove(&(table, key));
        } else if is_delete {
          log.lost_deletes.insert((table, key));
        }
      }
    }
    ended
  }

  fn write_soon(&self) {
    self.backend.write_soon();
  }

  fn write_tally(&self) -> Option<WriteTally> {
    self.backend.write_tally()
  }

  fn lost_deletes(&self, table: Table) -> Vec<String> {
    self
      .entry_log()
      .lost_deletes
      .iter()
      .filter(|(lost_table, _)| *lost_table == table)
      .map(|(_, key)| key.clone())
      .collect()
  }
}

impl<B: RecordBackend> SealedStore<B> {
  /// The entries of `table` whose key starts with `prefix`, through the table's directory.
  fn list_from(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let mut state = self.fresh();
    let mut keys = Vec::new();
    let start = prefix.to_string();
    self.directory(&mut state, table, |nodes| {
      sealed_index::scan(
        nodes,
        Bound::Included(&start),
        Direction::Forward,
        &mut |key, ()| {
          let is_inside = key.starts_with(prefix);
          if is_inside {
            keys.push(key.clone());
          }
          is_inside
        },
      )
    })?;
    let mut entries = Vec::with_capacity(keys.len());
    for key in keys {
      if let Some(value) = self.cached_value(&mut state, table, &key)? {
        entries.push((key, value));
      }
    }
    Ok(entries)
  }
}

/// An entry waiting to be sealed ([`SealedStore::write_named`]); the value is wiped on drop.
struct PendingEntry {
  table: String,
  key: String,
  value: Zeroizing<Vec<u8>>,
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

/// What a cached entry counts towards [`SealedStoreLimits::max_entry_bytes`].
fn entry_weight(key: &str, value_len: usize) -> usize {
  key.len() + value_len + 64
}

fn seal_record(keys: &LocalStoreKeys, id: &LookupId, record: &Record) -> Result<Vec<u8>, StoreError> {
  let plaintext =
    Zeroizing::new(serde_json::to_vec(record).map_err(|error| StoreError::Corrupt(error.to_string()))?);
  keys
    .seal(id, &plaintext, &mut OsRng)
    .map_err(|error| StoreError::Database(error.to_string()))
}

/// Lowercase hex, for backends with text keys and the catalog's progress.
pub(crate) fn format_id(id: &LookupId) -> String {
  id.0.iter().map(|byte| format!("{byte:02x}")).collect()
}

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
  use std::cell::Cell;
  use std::collections::BTreeMap;
  use std::sync::Mutex;

  use super::*;
  use crate::store::tests::{exercise_store, id, message};

  const CONVERSATIONS: i64 = 3;
  const MESSAGES_PER_CONVERSATION: i64 = 1_500;

  #[test]
  fn sealed_store_behaves_like_a_store_and_reopens() {
    let records = Records::default();
    let key = SymmetricKey::generate(&mut OsRng);
    let store = SealedStore::open_backend(&records, &key, SealedStoreLimits::default()).unwrap();
    exercise_store(&store);
    store.put(Table::Users, "9", b"recognizable plaintext").unwrap();
    assert!(
      records
        .map
        .lock()
        .unwrap()
        .values()
        .all(|sealed| !sealed.windows(12).any(|window| window == b"recognizable")),
      "nothing readable reaches the backend"
    );

    let reopened = SealedStore::open_backend(&records, &key, SealedStoreLimits::default()).unwrap();
    assert_eq!(
      reopened.get(Table::Users, "9").unwrap(),
      Some(b"recognizable plaintext".to_vec())
    );
    let wrong = SealedStore::open_backend(
      &records,
      &SymmetricKey::generate(&mut OsRng),
      SealedStoreLimits::default(),
    );
    assert!(matches!(wrong, Err(StoreError::WrongKey)));
  }

  #[test]
  fn ids_round_trip_through_hex() {
    let id = LookupId([0xab; LOOKUP_ID_LEN]);
    assert_eq!(parse_id(&format_id(&id)), Some(id));
    assert_eq!(parse_id("zz"), None);
  }

  mod lazy {
    use super::*;

    /// Issue #155: opening reads the catalog's state and nothing else, however long the history.
    #[test]
    fn opening_reads_no_record_but_the_catalogs_state() {
      let records = Records::default();
      let key = SymmetricKey::generate(&mut OsRng);
      fill(&SealedStore::open_backend(&records, &key, SealedStoreLimits::default()).unwrap());

      records.reads.set(0);
      let store = SealedStore::open_backend(&records, &key, SealedStoreLimits::minimal()).unwrap();
      assert_eq!(records.reads.get(), 1);

      let newest = store
        .message_by_server_id(id(1), id(server_id(1, MESSAGES_PER_CONVERSATION - 1)))
        .unwrap()
        .unwrap();
      assert_eq!(
        newest.client_message_id,
        client_id(1, MESSAGES_PER_CONVERSATION - 1)
      );
      assert!(
        records.reads.get() < 10,
        "a message is a root, a page and the record: {} reads",
        records.reads.get()
      );
    }

    #[test]
    fn caches_stay_within_their_limits() {
      let records = Records::default();
      let key = SymmetricKey::generate(&mut OsRng);
      fill(&SealedStore::open_backend(&records, &key, SealedStoreLimits::default()).unwrap());
      let limits = SealedStoreLimits::minimal();
      let store = SealedStore::open_backend(&records, &key, limits).unwrap();
      for conversation in 0..CONVERSATIONS {
        let all = store.messages(id(conversation), MessageFilter::All).unwrap();
        assert_eq!(all.len(), usize::try_from(MESSAGES_PER_CONVERSATION).unwrap());
      }
      let state = store.lock();
      assert!(state.messages.held() <= limits.max_messages);
      assert!(state.values.held() <= limits.max_entry_bytes);
    }

    /// A message sent moves from the pending end of the index to its server ID.
    #[test]
    fn a_sent_message_moves_to_its_place() {
      let records = Records::default();
      let store = SealedStore::open_backend(
        &records,
        &SymmetricKey::generate(&mut OsRng),
        SealedStoreLimits::default(),
      )
      .unwrap();
      let conversation = id(7);
      store
        .upsert_message(&message(conversation, 1, Some(10), None, 1))
        .unwrap();
      let mut pending = message(conversation, 2, None, None, 2);
      store.upsert_message(&pending).unwrap();
      store
        .upsert_message(&message(conversation, 3, Some(30), None, 3))
        .unwrap();
      pending.maybe_server_id = Some(id(20));
      store.upsert_message(&pending).unwrap();
      let order: Vec<u8> = store
        .messages(conversation, MessageFilter::All)
        .unwrap()
        .iter()
        .map(|message| message.client_message_id.as_bytes()[0])
        .collect();
      assert_eq!(order, [1, 2, 3]);
      assert!(
        store
          .messages_at(conversation, MessageSlot::Pending)
          .unwrap()
          .is_empty()
      );
    }
  }

  mod catalog {
    use super::*;

    /// Issue #155: a store written before the catalog existed reads back whole.
    #[test]
    fn a_store_written_before_the_catalog_reads_back_whole() {
      let (records, key) = legacy_store();
      let store = SealedStore::open_backend(&records, &key, SealedStoreLimits::default()).unwrap();
      check(&store);
    }

    /// Killed midway (here: stopped after two batches), the build goes on after the last batch it
    /// saved instead of starting over, and nothing is lost.
    #[test]
    fn an_interrupted_build_goes_on_where_it_stopped() {
      let (records, key) = legacy_store();
      let store = SealedStore::unbuilt(&records, &key, SealedStoreLimits::default()).unwrap();
      assert_eq!(store.build_catalog(Some(2)).unwrap(), BuildProgress::Interrupted);
      drop(store);

      records.reads.set(0);
      let total = records.map.lock().unwrap().len();
      let store = SealedStore::open_backend(&records, &key, SealedStoreLimits::default()).unwrap();
      assert!(
        records.reads.get() < total,
        "the first two batches are not read again: {} of {total}",
        records.reads.get()
      );
      check(&store);
      assert_eq!(store.read_catalog_state().unwrap(), Some(CatalogState::Complete));
    }

    /// Records as the versions before the catalog wrote them: the same records, no catalog.
    fn legacy_store() -> (Records, SymmetricKey) {
      let records = Records::default();
      let key = SymmetricKey::generate(&mut OsRng);
      let keys = LocalStoreKeys::derive(&key);
      let store = SealedStore::unbuilt(&records, &key, SealedStoreLimits::default()).unwrap();
      let write = |id: LookupId, record: Record| {
        let sealed = seal_record(&keys, &id, &record).unwrap();
        records.map.lock().unwrap().insert(id, sealed);
      };
      for conversation in 0..CONVERSATIONS {
        for n in 0..MESSAGES_PER_CONVERSATION {
          let stored = numbered(conversation, n);
          let id = store.message_id(stored.conversation_id, stored.client_message_id);
          write(
            id,
            Record::Message {
              message: Box::new(stored),
            },
          );
        }
      }
      for n in 0..100 {
        write(
          store.entry_id(Table::Users.name(), &n.to_string()),
          Record::Entry {
            table: Table::Users.name().into(),
            key: n.to_string(),
            value: Base64Bytes::new(vec![u8::try_from(n).unwrap(); 8]),
          },
        );
      }
      // A table of a newer version stays on disk, unlisted.
      write(
        store.entry_id("from_the_future", "1"),
        Record::Entry {
          table: "from_the_future".into(),
          key: "1".into(),
          value: Base64Bytes::new(vec![1]),
        },
      );
      (records, key)
    }

    fn check(store: &SealedStore<&Records>) {
      for conversation in 0..CONVERSATIONS {
        let all = store.messages(id(conversation), MessageFilter::All).unwrap();
        assert_eq!(all.len(), usize::try_from(MESSAGES_PER_CONVERSATION).unwrap());
        assert!(all.windows(2).all(|pair| pair[0].sort_key() < pair[1].sort_key()));
        assert_eq!(
          store
            .edge_server_id(id(conversation), MessageEdge::Newest)
            .unwrap(),
          Some(id(server_id(conversation, MESSAGES_PER_CONVERSATION - 1)))
        );
      }
      let users = store.list(Table::Users).unwrap();
      assert_eq!(users.len(), 100);
      assert_eq!(store.get(Table::Users, "42").unwrap(), Some(vec![42; 8]));
    }
  }

  /// A backend that keeps the sealed bytes in a map and counts the records it reads.
  #[derive(Default)]
  struct Records {
    map: Mutex<BTreeMap<LookupId, Vec<u8>>>,
    reads: Cell<usize>,
  }

  impl RecordBackend for &Records {
    fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
      self.map.lock().unwrap().insert(*id, sealed.to_vec());
      Ok(())
    }

    fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
      self.map.lock().unwrap().remove(id);
      Ok(())
    }

    fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
      self.reads.set(self.reads.get() + 1);
      Ok(self.map.lock().unwrap().get(id).cloned())
    }

    fn records_after(
      &self,
      maybe_after: Option<&LookupId>,
      limit: usize,
    ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
      let records = self.map.lock().unwrap();
      let range = match maybe_after {
        Some(after) => records.range((Bound::Excluded(*after), Bound::Unbounded)),
        None => records.range(..),
      };
      let batch: Vec<_> = range
        .take(limit)
        .map(|(id, sealed)| (*id, sealed.clone()))
        .collect();
      self.reads.set(self.reads.get() + batch.len());
      Ok(batch)
    }
  }

  fn fill(store: &SealedStore<&Records>) {
    crate::store::write_group(store, || {
      for conversation in 0..CONVERSATIONS {
        for n in 0..MESSAGES_PER_CONVERSATION {
          store.upsert_message(&numbered(conversation, n))?;
        }
      }
      for n in 0..100 {
        store.put(Table::Users, &n.to_string(), &[1; 64])?;
      }
      Ok::<_, StoreError>(())
    })
    .unwrap();
  }

  fn numbered(conversation: i64, n: i64) -> StoredMessage {
    let mut stored = message(id(conversation), 0, Some(server_id(conversation, n)), None, n);
    stored.client_message_id = client_id(conversation, n);
    stored
  }

  fn server_id(conversation: i64, n: i64) -> i64 {
    1_000_000 * (conversation + 1) + n
  }

  fn client_id(conversation: i64, n: i64) -> ClientMessageId {
    let mut bytes = [0; 16];
    bytes[..8].copy_from_slice(&conversation.to_be_bytes());
    bytes[8..].copy_from_slice(&n.to_be_bytes());
    ClientMessageId::from_bytes(bytes)
  }
}
