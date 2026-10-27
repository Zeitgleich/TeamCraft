//! The openmls provider storage in the local store, one record per storage entry (e2ee.md §9.5,
//! issue #424).
//!
//! openmls keeps a device's MLS state in its provider storage, a map from storage key to value:
//! every group's tree, context, epoch and message secrets, and the private keys of key packages.
//! Each entry is one record of [`Table::MlsStorage`]. A save writes the entries that changed since
//! the last save and deletes the ones openmls dropped, as one group of writes
//! ([`LocalStore::begin_writes`]): after a crash the store holds the state before the save or after
//! it, never a group half old, half new. Callers put the save in the same group as the records that
//! must agree with it (the cursor past a processed entry, the applied changes).
//!
//! Joining a group or processing a message writes what it changed, a few kilobytes, whatever the
//! number of groups. Before, the whole storage was one record rewritten after every change: a
//! catch-up that joined 300 groups wrote it 600 times, about 0.8 GB (docs/performance.md).
//!
//! [`SavedMlsStorage`] remembers what the store holds, so a save finds the changes without reading
//! the store. When the store may have lost writes it accepted (a group of writes rolled back,
//! [`LocalStore::write_tally`]) or a save failed, the next save writes every entry again and deletes
//! every key that may still be on disk, so memory and disk agree again as they did with one record.
//!
//! Versions before 1.0.9 kept the storage as one record ([`LEGACY_KEY`]); [`load_mls_storage`] moves
//! it to entries. The entries and the removal of the old record are one group of writes, the old
//! record removed last: as long as it is on disk it is what a start reads, and entries an earlier,
//! interrupted try left are replaced. The store layouts carry a version for the change (SQLite layout
//! 3, IndexedDB version 2), so an older version refuses the store instead of reading no MLS state.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, MutexGuard};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openmls_rust_crypto::OpenMlsRustCrypto;
use openmls_traits::OpenMlsProvider;
use zeroize::Zeroizing;

use crate::error::CoreError;
use crate::store::{LocalStore, Table, WriteTally, write_group};

/// The one record of the whole storage that versions before 1.0.9 wrote.
const LEGACY_KEY: &str = "provider";
/// Records of entries are keyed `entry/` and the storage key in unpadded base64url (which has no
/// `/`, so no entry is keyed [`LEGACY_KEY`]).
const ENTRY_PREFIX: &str = "entry/";

/// The provider storage's map, as openmls keeps it.
type StorageMap = HashMap<Vec<u8>, Vec<u8>>;
/// Entries as this module keeps them: the values hold secrets and are wiped on drop.
type Entries = HashMap<Vec<u8>, Zeroizing<Vec<u8>>>;

/// What the store holds of the MLS storage, as this device last wrote it; see the module docs.
/// A new one knows nothing: its first save writes every entry and deletes every other one.
pub struct SavedMlsStorage {
  inner: Mutex<Saved>,
}

struct Saved {
  /// The entries the store holds, by storage key.
  entries: Entries,
  /// Keys deleted by saves whose group of writes the store may still roll back (only on stores
  /// that can lose writes). A save after a lost write deletes them again.
  unconfirmed_deletes: BTreeSet<Vec<u8>>,
  /// The store's tally inside the last save's group of writes.
  maybe_tally: Option<WriteTally>,
  /// The store may not hold [`Self::entries`]: the next save writes everything.
  is_rewrite_due: bool,
  /// Bytes of records saves wrote (diagnostics).
  written_bytes: u64,
}

/// What one save writes: keys of the provider storage.
struct Changes<'a> {
  puts: Vec<&'a [u8]>,
  deletes: Vec<Vec<u8>>,
  /// Everything is written (a rewrite); `has_legacy`: the record of an older version is there.
  is_rewrite: bool,
  has_legacy: bool,
}

impl Default for SavedMlsStorage {
  fn default() -> Self {
    Self::new(Saved {
      entries: HashMap::new(),
      unconfirmed_deletes: BTreeSet::new(),
      maybe_tally: None,
      is_rewrite_due: true,
      written_bytes: 0,
    })
  }
}

impl SavedMlsStorage {
  /// Bytes of records the saves wrote since the storage was loaded (diagnostics; 0 if the lock is
  /// poisoned).
  pub fn written_bytes(&self) -> u64 {
    self.inner.lock().map_or(0, |saved| saved.written_bytes)
  }

  fn new(saved: Saved) -> Self {
    Self {
      inner: Mutex::new(saved),
    }
  }

  fn lock(&self) -> Result<MutexGuard<'_, Saved>, CoreError> {
    self.inner.lock().map_err(|_| poisoned())
  }
}

/// Loads the provider storage, moving the record of a version before 1.0.9 to entries first.
/// The returned [`SavedMlsStorage`] knows what the store holds.
pub fn load_mls_storage(store: &impl LocalStore) -> Result<(OpenMlsRustCrypto, SavedMlsStorage), CoreError> {
  let mut entries = Entries::new();
  let mut maybe_legacy = None;
  for (key, value) in store.list(Table::MlsStorage)? {
    let value = Zeroizing::new(value);
    if key == LEGACY_KEY {
      maybe_legacy = Some(value);
    } else if let Some(storage_key) = parse_entry_key(&key) {
      entries.insert(storage_key, value);
    } else {
      // Not written by this version; left alone.
      tracing::warn!("skipping an MLS storage record of an unknown kind");
    }
  }
  let mut unconfirmed_deletes = BTreeSet::new();
  if let Some(legacy) = maybe_legacy {
    let (migrated, stale) = migrate(store, &legacy, &entries)?;
    if store.write_tally().is_some() {
      unconfirmed_deletes.extend(stale);
    }
    entries = migrated;
  }
  let provider = OpenMlsRustCrypto::default();
  *provider.storage().values.write().map_err(|_| poisoned())? = entries
    .iter()
    .map(|(key, value)| (key.clone(), value.to_vec()))
    .collect();
  let saved = SavedMlsStorage::new(Saved {
    entries,
    unconfirmed_deletes,
    maybe_tally: store.write_tally(),
    is_rewrite_due: false,
    written_bytes: 0,
  });
  Ok((provider, saved))
}

/// Writes what changed in the provider storage since the last save, as one group of writes (see
/// the module docs). Call it after every MLS operation, inside the group of the records that must
/// agree with the state.
pub fn save_mls_storage(
  store: &impl LocalStore,
  provider: &OpenMlsRustCrypto,
  saved: &SavedMlsStorage,
) -> Result<(), CoreError> {
  let values = provider.storage().values.read().map_err(|_| poisoned())?;
  let mut state = saved.lock()?;
  state.note_tally(store.write_tally());
  let changes = state.changes(&values, store)?;
  if !changes.is_rewrite && changes.puts.is_empty() && changes.deletes.is_empty() {
    return Ok(());
  }
  let written = write_group(store, || -> Result<_, CoreError> {
    // Inside the group: a commit the store counts from here on ends this group too.
    let tally = store.write_tally();
    let mut bytes = 0u64;
    for key in &changes.puts {
      let Some(value) = values.get(*key) else {
        continue;
      };
      let record_key = entry_key(key);
      store.put(Table::MlsStorage, &record_key, value)?;
      bytes = bytes.saturating_add(u64::try_from(record_key.len() + value.len()).unwrap_or(u64::MAX));
    }
    for key in &changes.deletes {
      store.delete(Table::MlsStorage, &entry_key(key))?;
    }
    if changes.has_legacy {
      // A store of a version before 1.0.9 this device was set up on again.
      store.delete(Table::MlsStorage, LEGACY_KEY)?;
    }
    Ok((tally, bytes))
  });
  match written {
    Ok((tally, bytes)) => {
      state.saved(&changes, &values, tally);
      state.written_bytes = state.written_bytes.saturating_add(bytes);
      drop(state);
      // Replaced and deleted entries (old epoch secrets, spent key package private keys) sit in
      // free pages and the write-ahead log until the store clears them (#417).
      store.purge_freed_soon();
      Ok(())
    },
    Err(error) => {
      state.failed(changes.deletes);
      Err(error)
    },
  }
}

/// The size of the provider storage, keys and values (diagnostics; 0 if the lock is poisoned).
pub fn mls_storage_len(provider: &OpenMlsRustCrypto) -> usize {
  provider.storage().values.read().map_or(0, |values| {
    values
      .iter()
      .map(|(key, value)| key.len().saturating_add(value.len()))
      .sum()
  })
}

impl Saved {
  /// Takes in what became of the store's groups of writes since the last save: a lost one means
  /// the store may not hold what this remembers; a committed one confirms the earlier deletes.
  fn note_tally(&mut self, maybe_now: Option<WriteTally>) {
    match (self.maybe_tally, maybe_now) {
      (Some(before), Some(now)) if now.lost != before.lost => {
        tracing::warn!("the local store lost writes; the MLS state is written again in full");
        self.is_rewrite_due = true;
      },
      // A commit needs every open group ended, so the last save's group is on disk.
      (Some(before), Some(now)) if now.committed != before.committed => self.unconfirmed_deletes.clear(),
      // The store keeps every write it accepted.
      (_, None) => self.unconfirmed_deletes.clear(),
      _ => {},
    }
  }

  /// The keys to write and to delete: what differs from the saved entries, or everything when a
  /// rewrite is due.
  fn changes<'a>(&self, values: &'a StorageMap, store: &impl LocalStore) -> Result<Changes<'a>, CoreError> {
    if !self.is_rewrite_due {
      let puts = values
        .iter()
        .filter(|(key, value)| {
          self
            .entries
            .get(key.as_slice())
            .is_none_or(|saved| saved.as_slice() != value.as_slice())
        })
        .map(|(key, _)| key.as_slice())
        .collect();
      let deletes = self
        .entries
        .keys()
        .filter(|key| !values.contains_key(key.as_slice()))
        .cloned()
        .collect();
      return Ok(Changes {
        puts,
        deletes,
        is_rewrite: false,
        has_legacy: false,
      });
    }
    // Every key that may be on disk: what the store lists (the disk itself for SQLCipher), what
    // this remembers, and deletes a lost group may have undone (a sealed store's cache doesn't
    // list those).
    let mut maybe_stored: BTreeSet<Vec<u8>> = self.unconfirmed_deletes.clone();
    maybe_stored.extend(self.entries.keys().cloned());
    let mut has_legacy = false;
    for (key, value) in store.list(Table::MlsStorage)? {
      drop(Zeroizing::new(value));
      if key == LEGACY_KEY {
        has_legacy = true;
      } else if let Some(storage_key) = parse_entry_key(&key) {
        maybe_stored.insert(storage_key);
      }
    }
    Ok(Changes {
      puts: values.keys().map(Vec::as_slice).collect(),
      deletes: maybe_stored
        .into_iter()
        .filter(|key| !values.contains_key(key))
        .collect(),
      is_rewrite: true,
      has_legacy,
    })
  }

  /// The save's group of writes went through; an outer group may still roll it back, which
  /// [`Self::note_tally`] finds out.
  fn saved(&mut self, changes: &Changes<'_>, values: &StorageMap, maybe_tally: Option<WriteTally>) {
    if changes.is_rewrite {
      self.entries.clear();
    }
    for key in &changes.puts {
      if let Some(value) = values.get(*key) {
        self.entries.insert(key.to_vec(), Zeroizing::new(value.clone()));
      }
    }
    for key in &changes.deletes {
      self.entries.remove(key);
    }
    if maybe_tally.is_some() {
      self.unconfirmed_deletes.extend(changes.deletes.iter().cloned());
    }
    self.maybe_tally = maybe_tally;
    self.is_rewrite_due = false;
  }

  /// The save failed: what reached the store is unknown.
  fn failed(&mut self, deletes: Vec<Vec<u8>>) {
    self.unconfirmed_deletes.extend(deletes);
    self.is_rewrite_due = true;
  }
}

/// Moves the record of a version before 1.0.9 to entries (see the module docs). `found` are
/// entries already in the store (left by an interrupted earlier try): the old record wins. Returns
/// the entries and the keys of `found` it deleted.
fn migrate(
  store: &impl LocalStore,
  legacy: &[u8],
  found: &Entries,
) -> Result<(Entries, Vec<Vec<u8>>), CoreError> {
  let entries = decode_legacy(legacy)?;
  let stale: Vec<Vec<u8>> = found
    .keys()
    .filter(|key| !entries.contains_key(key.as_slice()))
    .cloned()
    .collect();
  write_group(store, || -> Result<(), CoreError> {
    for (key, value) in &entries {
      store.put(Table::MlsStorage, &entry_key(key), value)?;
    }
    for key in &stale {
      store.delete(Table::MlsStorage, &entry_key(key))?;
    }
    // Last: until the entries are on disk, the old record is what a start reads.
    store.delete(Table::MlsStorage, LEGACY_KEY)?;
    Ok(())
  })?;
  // The old record's bytes go from the free pages and the log too.
  store.purge_freed_soon();
  tracing::info!(
    entries = entries.len(),
    "moved the MLS state from one record to one record per entry"
  );
  Ok((entries, stale))
}

/// The entries of the one record versions before 1.0.9 wrote: length-prefixed keys and values.
fn decode_legacy(encoded: &[u8]) -> Result<Entries, CoreError> {
  let mut entries = Entries::new();
  let mut rest = encoded;
  while !rest.is_empty() {
    let key = take_chunk(&mut rest)?;
    let value = Zeroizing::new(take_chunk(&mut rest)?);
    entries.insert(key, value);
  }
  Ok(entries)
}

fn take_chunk(rest: &mut &[u8]) -> Result<Vec<u8>, CoreError> {
  let corrupt = || CoreError::Corrupt("truncated MLS storage snapshot".into());
  let (length, tail) = rest.split_first_chunk::<4>().ok_or_else(corrupt)?;
  let length = u32::from_be_bytes(*length) as usize;
  let chunk = tail.get(..length).ok_or_else(corrupt)?;
  *rest = &tail[length..];
  Ok(chunk.to_vec())
}

fn entry_key(storage_key: &[u8]) -> String {
  format!("{ENTRY_PREFIX}{}", URL_SAFE_NO_PAD.encode(storage_key))
}

fn parse_entry_key(key: &str) -> Option<Vec<u8>> {
  URL_SAFE_NO_PAD.decode(key.strip_prefix(ENTRY_PREFIX)?).ok()
}

fn poisoned() -> CoreError {
  CoreError::Corrupt("MLS storage lock poisoned".into())
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use teamcraft_proto::{ClientMessageId, ConversationId, MessageId};

  use super::*;
  use crate::store::{MediaRange, MemoryStore, MessageFilter, StoreError, StoredMessage};

  #[test]
  fn entry_keys_round_trip_and_never_name_the_old_record() {
    for storage_key in [&b""[..], b"Tree\x00\x01", &[0xff; 300]] {
      let key = entry_key(storage_key);
      assert_ne!(key, LEGACY_KEY);
      assert_eq!(parse_entry_key(&key).as_deref(), Some(storage_key));
    }
    assert_eq!(parse_entry_key(LEGACY_KEY), None);
    assert_eq!(parse_entry_key("entry/not base64!"), None);
  }

  #[test]
  fn loading_reads_back_what_was_saved() {
    let store = MemoryStore::new();
    let provider = provider_with(&[(b"a", b"1"), (b"b", b"2")]);
    save_mls_storage(&store, &provider, &SavedMlsStorage::default()).unwrap();

    let (loaded, _) = load_mls_storage(&store).unwrap();
    assert_eq!(entries_of(&loaded), entries_of(&provider));
    let (empty, _) = load_mls_storage(&MemoryStore::new()).unwrap();
    assert_eq!(entries_of(&empty), Vec::new());
  }

  /// A change to one group's entries writes those entries, not every group's (issue #424).
  #[test]
  fn a_save_writes_only_what_changed() {
    let store = FlakyStore::default();
    let provider = provider_with(&[(b"a", b"1"), (b"b", b"2"), (b"c", b"3")]);
    let saved = SavedMlsStorage::default();
    save_mls_storage(&store, &provider, &saved).unwrap();
    assert_eq!(store.take_counts(), (3, 0));

    set(&provider, b"b", Some(b"22"));
    set(&provider, b"c", None);
    set(&provider, b"d", Some(b"4"));
    save_mls_storage(&store, &provider, &saved).unwrap();
    assert_eq!(store.take_counts(), (2, 1), "b and d written, c deleted");
    assert_eq!(durable_entries(&store), entries_of(&provider));

    save_mls_storage(&store, &provider, &saved).unwrap();
    assert_eq!(store.take_counts(), (0, 0), "nothing changed, nothing written");
    assert_eq!(store.groups(), 2, "one group of writes per save that wrote");
  }

  /// A device set up again on a store with MLS state (of any version) starts from its own.
  #[test]
  fn a_new_saved_state_replaces_whatever_the_store_held() {
    let store = MemoryStore::new();
    store
      .put(Table::MlsStorage, LEGACY_KEY, &legacy(&[(b"old", b"x")]))
      .unwrap();
    store.put(Table::MlsStorage, &entry_key(b"stale"), b"y").unwrap();
    let provider = provider_with(&[(b"new", b"z")]);

    save_mls_storage(&store, &provider, &SavedMlsStorage::default()).unwrap();

    let keys: Vec<String> = store
      .list(Table::MlsStorage)
      .unwrap()
      .into_iter()
      .map(|(key, _)| key)
      .collect();
    assert_eq!(keys, [entry_key(b"new")]);
  }

  mod migration {
    use super::*;

    #[test]
    fn the_record_of_an_older_version_moves_to_entries() {
      let store = FlakyStore::default();
      store
        .put(
          Table::MlsStorage,
          LEGACY_KEY,
          &legacy(&[(b"a", b"1"), (b"b", b"2")]),
        )
        .unwrap();
      store.take_counts();

      let (provider, saved) = load_mls_storage(&store).unwrap();

      assert_eq!(entries_of(&provider), [entry(b"a", b"1"), entry(b"b", b"2")]);
      assert_eq!(durable_entries(&store), entries_of(&provider));
      assert_eq!(store.durable.get(Table::MlsStorage, LEGACY_KEY).unwrap(), None);
      assert_eq!(
        store.groups(),
        1,
        "entries and the old record's removal are one group"
      );
      // Saved as it was moved: nothing is written again.
      store.take_counts();
      save_mls_storage(&store, &provider, &saved).unwrap();
      assert_eq!(store.take_counts(), (0, 0));
    }

    #[test]
    fn a_moved_store_is_not_moved_again() {
      let store = FlakyStore::default();
      store
        .put(Table::MlsStorage, LEGACY_KEY, &legacy(&[(b"a", b"1")]))
        .unwrap();
      load_mls_storage(&store).unwrap();
      store.take_counts();

      let (provider, _) = load_mls_storage(&store).unwrap();

      assert_eq!(entries_of(&provider), [entry(b"a", b"1")]);
      assert_eq!(store.take_counts(), (0, 0));
    }

    /// Entries next to the old record: a try that didn't finish on a store whose groups aren't
    /// atomic. The old record still decides.
    #[test]
    fn an_unfinished_move_is_done_again_from_the_old_record() {
      let store = FlakyStore::default();
      store
        .put(
          Table::MlsStorage,
          LEGACY_KEY,
          &legacy(&[(b"a", b"1"), (b"b", b"2")]),
        )
        .unwrap();
      store.put(Table::MlsStorage, &entry_key(b"a"), b"other").unwrap();
      store.put(Table::MlsStorage, &entry_key(b"stale"), b"x").unwrap();

      let (provider, _) = load_mls_storage(&store).unwrap();

      assert_eq!(entries_of(&provider), [entry(b"a", b"1"), entry(b"b", b"2")]);
      assert_eq!(durable_entries(&store), entries_of(&provider));
      assert_eq!(store.durable.get(Table::MlsStorage, LEGACY_KEY).unwrap(), None);
    }

    /// A move whose group of writes failed leaves the old record, and the next start moves it.
    #[test]
    fn a_failed_move_keeps_the_old_record() {
      let store = FlakyStore::default();
      let old = legacy(&[(b"a", b"1"), (b"b", b"2")]);
      store.put(Table::MlsStorage, LEGACY_KEY, &old).unwrap();
      store.fail_next_group();

      assert!(load_mls_storage(&store).is_err());

      assert_eq!(
        store.durable.list(Table::MlsStorage).unwrap(),
        [(LEGACY_KEY.to_string(), old)]
      );
      let restarted = FlakyStore::on(&store.durable);
      let (provider, _) = load_mls_storage(&restarted).unwrap();
      assert_eq!(entries_of(&provider), [entry(b"a", b"1"), entry(b"b", b"2")]);
      assert_eq!(durable_entries(&restarted), entries_of(&provider));
    }

    #[test]
    fn a_damaged_old_record_is_an_error() {
      let store = MemoryStore::new();
      store
        .put(Table::MlsStorage, LEGACY_KEY, &[0, 0, 0, 9, 1])
        .unwrap();
      assert!(matches!(load_mls_storage(&store), Err(CoreError::Corrupt(_))));
    }
  }

  mod lost_writes {
    use super::*;

    /// A group the store rolled back after the save went through (an outer group whose commit
    /// failed): the next save writes everything again, and deletes again what the lost group
    /// deleted, though the store's cache doesn't list it any more (a sealed store's).
    #[test]
    fn after_a_lost_group_the_next_save_makes_the_disk_match_again() {
      let store = FlakyStore::default();
      let provider = provider_with(&[(b"a", b"1"), (b"b", b"2")]);
      let saved = SavedMlsStorage::default();
      save_mls_storage(&store, &provider, &saved).unwrap();

      set(&provider, b"a", Some(b"11"));
      set(&provider, b"b", None);
      store.fail_next_group();
      let lost = write_group(&store, || save_mls_storage(&store, &provider, &saved));
      assert!(lost.is_err());
      assert_eq!(durable_entries(&store), [entry(b"a", b"1"), entry(b"b", b"2")]);

      set(&provider, b"c", Some(b"3"));
      store.take_counts();
      save_mls_storage(&store, &provider, &saved).unwrap();

      assert_eq!(durable_entries(&store), entries_of(&provider));
      assert_eq!(store.take_counts(), (2, 1), "a and c written, b deleted again");
      // Back to writing what changed.
      set(&provider, b"c", Some(b"33"));
      save_mls_storage(&store, &provider, &saved).unwrap();
      assert_eq!(store.take_counts(), (1, 0));
    }

    /// A save whose own group failed: unknown what reached the store, so the next one writes all.
    #[test]
    fn after_a_failed_save_the_next_one_writes_everything() {
      let store = FlakyStore::default();
      let provider = provider_with(&[(b"a", b"1"), (b"b", b"2")]);
      let saved = SavedMlsStorage::default();
      save_mls_storage(&store, &provider, &saved).unwrap();

      set(&provider, b"b", None);
      store.fail_next_group();
      assert!(save_mls_storage(&store, &provider, &saved).is_err());
      store.take_counts();
      save_mls_storage(&store, &provider, &saved).unwrap();

      assert_eq!(store.take_counts(), (1, 1));
      assert_eq!(durable_entries(&store), entries_of(&provider));
    }

    /// Deletes are remembered only until a commit shows their group reached the disk.
    #[test]
    fn deletes_are_forgotten_once_a_later_group_committed() {
      let store = FlakyStore::default();
      let provider = provider_with(&[(b"a", b"1"), (b"b", b"2")]);
      let saved = SavedMlsStorage::default();
      save_mls_storage(&store, &provider, &saved).unwrap();
      set(&provider, b"b", None);
      write_group(&store, || -> Result<(), CoreError> {
        save_mls_storage(&store, &provider, &saved)?;
        set(&provider, b"a", None);
        save_mls_storage(&store, &provider, &saved)?;
        // Both saves' groups are still open: their deletes may still be lost.
        assert_eq!(saved.lock()?.unconfirmed_deletes.len(), 2);
        Ok(())
      })
      .unwrap();
      set(&provider, b"c", Some(b"3"));
      save_mls_storage(&store, &provider, &saved).unwrap();
      assert!(saved.lock().unwrap().unconfirmed_deletes.is_empty());
    }

    /// A store that keeps every write it accepted needs no deletes remembered.
    #[test]
    fn a_store_without_a_tally_keeps_no_deletes() {
      let store = MemoryStore::new();
      let provider = provider_with(&[(b"a", b"1"), (b"b", b"2")]);
      let saved = SavedMlsStorage::default();
      save_mls_storage(&store, &provider, &saved).unwrap();
      set(&provider, b"b", None);
      save_mls_storage(&store, &provider, &saved).unwrap();
      assert!(saved.lock().unwrap().unconfirmed_deletes.is_empty());
      assert_eq!(store.get(Table::MlsStorage, &entry_key(b"b")).unwrap(), None);
    }
  }

  type Entry = (Vec<u8>, Vec<u8>);

  fn entry(key: &[u8], value: &[u8]) -> Entry {
    (key.to_vec(), value.to_vec())
  }

  fn provider_with(entries: &[(&[u8], &[u8])]) -> OpenMlsRustCrypto {
    let provider = OpenMlsRustCrypto::default();
    for (key, value) in entries {
      set(&provider, key, Some(value));
    }
    provider
  }

  fn set(provider: &OpenMlsRustCrypto, key: &[u8], maybe_value: Option<&[u8]>) {
    let mut values = provider.storage().values.write().unwrap();
    match maybe_value {
      Some(value) => values.insert(key.to_vec(), value.to_vec()),
      None => values.remove(key),
    };
  }

  fn entries_of(provider: &OpenMlsRustCrypto) -> Vec<Entry> {
    let mut entries: Vec<Entry> = provider
      .storage()
      .values
      .read()
      .unwrap()
      .iter()
      .map(|(key, value)| (key.clone(), value.clone()))
      .collect();
    entries.sort();
    entries
  }

  /// The entries on the fake store's disk.
  fn durable_entries(store: &FlakyStore) -> Vec<Entry> {
    let mut entries: Vec<Entry> = store
      .durable
      .list(Table::MlsStorage)
      .unwrap()
      .into_iter()
      .map(|(key, value)| (parse_entry_key(&key).unwrap(), value))
      .collect();
    entries.sort();
    entries
  }

  /// The one record of a version before 1.0.9.
  fn legacy(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for (key, value) in entries {
      for chunk in [key, value] {
        encoded.extend_from_slice(&u32::try_from(chunk.len()).unwrap().to_be_bytes());
        encoded.extend_from_slice(chunk);
      }
    }
    encoded
  }

  /// A store like the sealed SQLite one: reads come from a cache that keeps every write, the disk
  /// ([`Self::durable`]) gets a group when it commits, and a group can be made to fail, which the
  /// disk rolls back and the tally counts. Counts the MLS puts and deletes.
  #[derive(Default)]
  struct FlakyStore {
    cache: MemoryStore,
    durable: MemoryStore,
    state: Mutex<FlakyState>,
  }

  #[derive(Default)]
  struct FlakyState {
    depth: u32,
    group: Vec<(String, Option<Vec<u8>>)>,
    is_next_group_failing: bool,
    tally: WriteTally,
    groups: usize,
    puts: usize,
    deletes: usize,
  }

  impl FlakyStore {
    /// A store whose disk starts as `durable` holds it (a restart).
    fn on(durable: &MemoryStore) -> Self {
      let store = Self::default();
      for (key, value) in durable.list(Table::MlsStorage).unwrap() {
        store.cache.put(Table::MlsStorage, &key, &value).unwrap();
        store.durable.put(Table::MlsStorage, &key, &value).unwrap();
      }
      store
    }

    fn fail_next_group(&self) {
      self.state.lock().unwrap().is_next_group_failing = true;
    }

    /// MLS puts and deletes since the last call.
    fn take_counts(&self) -> (usize, usize) {
      let mut state = self.state.lock().unwrap();
      (
        std::mem::take(&mut state.puts),
        std::mem::take(&mut state.deletes),
      )
    }

    /// Outermost groups ended so far.
    fn groups(&self) -> usize {
      self.state.lock().unwrap().groups
    }

    fn write(&self, table: Table, key: &str, maybe_value: Option<&[u8]>) -> Result<(), StoreError> {
      assert_eq!(table, Table::MlsStorage, "only MLS records here");
      match maybe_value {
        Some(value) => self.cache.put(table, key, value)?,
        None => self.cache.delete(table, key)?,
      }
      let mut state = self.state.lock().unwrap();
      match maybe_value {
        Some(_) => state.puts += 1,
        None => state.deletes += 1,
      }
      let write = (key.to_string(), maybe_value.map(<[u8]>::to_vec));
      if state.depth > 0 {
        state.group.push(write);
        Ok(())
      } else {
        apply(&self.durable, &write)
      }
    }
  }

  fn apply(store: &MemoryStore, (key, maybe_value): &(String, Option<Vec<u8>>)) -> Result<(), StoreError> {
    match maybe_value {
      Some(value) => store.put(Table::MlsStorage, key, value),
      None => store.delete(Table::MlsStorage, key),
    }
  }

  impl LocalStore for FlakyStore {
    fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
      self.cache.get(table, key)
    }

    fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
      self.write(table, key, Some(value))
    }

    fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
      self.write(table, key, None)
    }

    fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
      self.cache.list(table)
    }

    fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
      self.cache.upsert_message(message)
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

    fn media_messages(
      &self,
      conversation: ConversationId,
      range: MediaRange,
      limit: usize,
    ) -> Result<Vec<StoredMessage>, StoreError> {
      self.cache.media_messages(conversation, range, limit)
    }

    fn message_by_server_id(
      &self,
      conversation: ConversationId,
      server_id: MessageId,
    ) -> Result<Option<StoredMessage>, StoreError> {
      self.cache.message_by_server_id(conversation, server_id)
    }

    fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
      self.cache.delete_messages(conversation)
    }

    fn delete_message(
      &self,
      conversation: ConversationId,
      client_message_id: ClientMessageId,
    ) -> Result<(), StoreError> {
      self.cache.delete_message(conversation, client_message_id)
    }

    fn begin_writes(&self) -> Result<(), StoreError> {
      self.state.lock().unwrap().depth += 1;
      Ok(())
    }

    fn end_writes(&self) -> Result<(), StoreError> {
      let mut state = self.state.lock().unwrap();
      state.depth -= 1;
      if state.depth > 0 {
        return Ok(());
      }
      state.groups += 1;
      let group = std::mem::take(&mut state.group);
      if std::mem::take(&mut state.is_next_group_failing) {
        state.tally.lost += 1;
        return Err(StoreError::GroupFailed("made to fail".into()));
      }
      state.tally.committed += 1;
      for write in &group {
        apply(&self.durable, write)?;
      }
      Ok(())
    }

    fn write_tally(&self) -> Option<WriteTally> {
      Some(self.state.lock().unwrap().tally)
    }
  }
}
