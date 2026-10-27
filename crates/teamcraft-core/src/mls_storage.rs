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
//! the store. When the disk may hold something else ([`LocalStore::write_tally`]: a group of writes
//! the disk rolled back, a commit of another process) or a save failed, the next save writes every
//! entry again and deletes every key that may still be on disk, the deletes the disk lost included
//! ([`LocalStore::lost_deletes`]): memory and disk agree again, as they did with one record. A
//! client loaded on a store that lost writes (reopened in the same process) starts that way too.
//!
//! Versions before 1.0.9 kept the storage as one record ([`LEGACY_KEY`]). [`load_mls_storage`] moves
//! it to entries in one group of writes and leaves [`MOVED_MARKER`] in its place, which every
//! earlier version fails to read (instead of reading no MLS state, losing every group, and later
//! saving a near-empty record). The store layouts carry a version for the change as well (SQLite
//! layout 3, IndexedDB version 2), so versions that check it refuse the store before that. Entries
//! next to an old record of another kind are never dropped: that record is set aside.

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
/// What [`LEGACY_KEY`] holds once the state is stored per entry: a length prefix far beyond the
/// record, so every version before 1.0.9 (which reads length-prefixed keys and values from it)
/// reports the store as damaged instead of reading no MLS state.
const MOVED_MARKER: &[u8] = b"\xff\xff\xff\xffMLS state stored per entry since TeamCraft 1.0.9 (#424)";
/// Records of entries are keyed `entry/` and the storage key in unpadded base64url (which has no
/// `/`, so no entry is keyed [`LEGACY_KEY`]).
const ENTRY_PREFIX: &str = "entry/";
/// Where an old record found next to entries is kept (numbered), never read again.
const SET_ASIDE_PREFIX: &str = "set_aside/";

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
  /// The store's tally at the last save (or the load).
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
  /// Everything is written (a rewrite); `old_record`: what the store holds under [`LEGACY_KEY`].
  is_rewrite: bool,
  old_record: OldRecord,
}

/// What a store holds under [`LEGACY_KEY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OldRecord {
  Missing,
  /// [`MOVED_MARKER`].
  Marker,
  /// The whole storage as a version before 1.0.9 wrote it.
  Snapshot,
}

/// The MLS records of a store as listed.
struct Listed {
  entries: Entries,
  maybe_old_record: Option<Zeroizing<Vec<u8>>>,
  set_aside: usize,
}

impl Default for SavedMlsStorage {
  fn default() -> Self {
    Self::new(Saved {
      entries: HashMap::new(),
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
  let Listed {
    mut entries,
    maybe_old_record,
    set_aside,
  } = list_records(store)?;
  match maybe_old_record {
    Some(old) if old.as_slice() == MOVED_MARKER => {},
    Some(old) if entries.is_empty() => entries = migrate(store, &old)?,
    // Only an older version writing next to moved entries gets here (all stores write a group
    // whole, the move included): the entries are the device's state, the record is kept apart.
    Some(old) => {
      tracing::warn!("an MLS record of an older version was found next to the entries; set aside");
      write_group(store, || -> Result<(), CoreError> {
        store.put(Table::MlsStorage, &set_aside_key(set_aside), &old)?;
        store.put(Table::MlsStorage, LEGACY_KEY, MOVED_MARKER)?;
        Ok(())
      })?;
    },
    // Moved before the marker existed, or a store this version set up.
    None => store.put(Table::MlsStorage, LEGACY_KEY, MOVED_MARKER)?,
  }
  let provider = OpenMlsRustCrypto::default();
  *provider.storage().values.write().map_err(|_| poisoned())? = entries
    .iter()
    .map(|(key, value)| (key.clone(), value.to_vec()))
    .collect();
  let maybe_tally = store.write_tally();
  // The store already lost writes (a client reopened in the same process) or another process
  // wrote: what it lists may not be what the disk holds.
  let is_rewrite_due = maybe_tally.is_some_and(|tally| tally.lost > 0 || tally.foreign > 0);
  let saved = SavedMlsStorage::new(Saved {
    entries,
    maybe_tally,
    is_rewrite_due,
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
    if changes.is_rewrite && changes.old_record != OldRecord::Marker {
      // A store set up again (an old record of any version goes) or one that lost the marker.
      store.put(Table::MlsStorage, LEGACY_KEY, MOVED_MARKER)?;
    }
    Ok(bytes)
  });
  match written {
    Ok(bytes) => {
      state.saved(&changes, &values, store.write_tally());
      state.written_bytes = state.written_bytes.saturating_add(bytes);
      drop(state);
      // Replaced and deleted entries (old epoch secrets, spent key package private keys) sit in
      // free pages and the write-ahead log until the store clears them (#417).
      store.purge_freed_soon();
      Ok(())
    },
    Err(error) => {
      state.failed();
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
  /// A moved tally since the last save means the disk may hold something else: writes it lost,
  /// or another process's commits.
  fn note_tally(&mut self, maybe_now: Option<WriteTally>) {
    if maybe_now != self.maybe_tally {
      if self.maybe_tally.is_some() {
        tracing::warn!(
          "the local store lost writes or another process wrote; the MLS state is written in full"
        );
      }
      self.is_rewrite_due = true;
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
        old_record: OldRecord::Marker,
      });
    }
    // Every key that may be on disk: what the store lists (the disk itself for SQLCipher), what
    // this remembers, and deletes the disk lost (a sealed store's cache doesn't list those).
    let listed = list_records(store)?;
    let mut maybe_stored: BTreeSet<Vec<u8>> = self.entries.keys().cloned().collect();
    maybe_stored.extend(listed.entries.into_keys());
    maybe_stored.extend(
      store
        .lost_deletes(Table::MlsStorage)
        .iter()
        .filter_map(|key| parse_entry_key(key)),
    );
    let old_record = match listed.maybe_old_record {
      None => OldRecord::Missing,
      Some(old) if old.as_slice() == MOVED_MARKER => OldRecord::Marker,
      Some(_) => OldRecord::Snapshot,
    };
    Ok(Changes {
      puts: values.keys().map(Vec::as_slice).collect(),
      deletes: maybe_stored
        .into_iter()
        .filter(|key| !values.contains_key(key))
        .collect(),
      is_rewrite: true,
      old_record,
    })
  }

  /// The save's group of writes went through; an outer group may still roll it back, which the
  /// store's tally tells the next save.
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
    self.maybe_tally = maybe_tally;
    self.is_rewrite_due = false;
  }

  /// The save failed: what reached the store is unknown, so the next save writes everything, on
  /// any store (one without a tally too).
  fn failed(&mut self) {
    self.is_rewrite_due = true;
  }
}

/// The MLS records of `store`: entries, the record under [`LEGACY_KEY`], and how many were set
/// aside. Records of other kinds (a newer version's) are left alone.
fn list_records(store: &impl LocalStore) -> Result<Listed, CoreError> {
  let mut listed = Listed {
    entries: Entries::new(),
    maybe_old_record: None,
    set_aside: 0,
  };
  for (key, value) in store.list(Table::MlsStorage)? {
    let value = Zeroizing::new(value);
    if key == LEGACY_KEY {
      listed.maybe_old_record = Some(value);
    } else if let Some(storage_key) = parse_entry_key(&key) {
      listed.entries.insert(storage_key, value);
    } else if key.starts_with(SET_ASIDE_PREFIX) {
      listed.set_aside += 1;
    } else {
      tracing::warn!("skipping an MLS storage record of an unknown kind");
    }
  }
  Ok(listed)
}

/// Moves the record of a version before 1.0.9 to entries (see the module docs) and returns them.
/// One group of writes: the entries, then [`MOVED_MARKER`] in the old record's place.
fn migrate(store: &impl LocalStore, legacy: &[u8]) -> Result<Entries, CoreError> {
  let entries = decode_legacy(legacy)?;
  write_group(store, || -> Result<(), CoreError> {
    for (key, value) in &entries {
      store.put(Table::MlsStorage, &entry_key(key), value)?;
    }
    // Last: until the entries are on disk, the old record is what a start reads.
    store.put(Table::MlsStorage, LEGACY_KEY, MOVED_MARKER)?;
    Ok(())
  })?;
  // The old record's bytes go from the free pages and the log too.
  store.purge_freed_soon();
  tracing::info!(
    entries = entries.len(),
    "moved the MLS state from one record to one record per entry"
  );
  Ok(entries)
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

fn set_aside_key(index: usize) -> String {
  format!("{SET_ASIDE_PREFIX}{LEGACY_KEY}-{}", index + 1)
}

fn poisoned() -> CoreError {
  CoreError::Corrupt("MLS storage lock poisoned".into())
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeSet;
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
    assert_eq!(parse_entry_key(&set_aside_key(0)), None);
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

    assert_eq!(
      store.list(Table::MlsStorage).unwrap(),
      [
        (entry_key(b"new"), b"z".to_vec()),
        (LEGACY_KEY.to_string(), MOVED_MARKER.to_vec())
      ]
    );
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
      assert_eq!(
        store.durable.get(Table::MlsStorage, LEGACY_KEY).unwrap(),
        Some(MOVED_MARKER.to_vec()),
        "the marker in the old record's place"
      );
      assert_eq!(
        store.groups(),
        1,
        "entries and the old record's replacement are one group"
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
      assert_eq!(store.marker_writes(), 1);
    }

    /// Issue #424 review, H1: every version before 1.0.9 reads the old record as length-prefixed
    /// keys and values (this module's `decode_legacy` is that reader); the marker makes it fail
    /// instead of reading no MLS state.
    #[test]
    fn every_older_version_fails_to_read_the_marker() {
      assert!(matches!(decode_legacy(MOVED_MARKER), Err(CoreError::Corrupt(_))));
    }

    /// A store this version set up, or moved before the marker existed, gets the marker on load.
    #[test]
    fn a_store_without_the_marker_gets_it() {
      let store = MemoryStore::new();
      store.put(Table::MlsStorage, &entry_key(b"a"), b"1").unwrap();

      let (provider, _) = load_mls_storage(&store).unwrap();

      assert_eq!(entries_of(&provider), [entry(b"a", b"1")]);
      assert_eq!(
        store.get(Table::MlsStorage, LEGACY_KEY).unwrap(),
        Some(MOVED_MARKER.to_vec())
      );
    }

    /// Issue #424 review, H1: an old record next to entries can only come from an older version
    /// writing after the move (before the marker existed). The entries are the device's groups:
    /// they stay, the record is set aside, nothing is deleted.
    #[test]
    fn entries_next_to_an_old_record_stay_and_the_record_is_set_aside() {
      let store = FlakyStore::default();
      store
        .put(Table::MlsStorage, &entry_key(b"group"), b"epoch 7")
        .unwrap();
      let written_by_older = legacy(&[(b"key package", b"new")]);
      store
        .put(Table::MlsStorage, LEGACY_KEY, &written_by_older)
        .unwrap();
      store.take_counts();

      let (provider, _) = load_mls_storage(&store).unwrap();

      assert_eq!(entries_of(&provider), [entry(b"group", b"epoch 7")]);
      assert_eq!(store.take_counts(), (0, 0), "no entry written or deleted");
      assert_eq!(
        store.durable.get(Table::MlsStorage, &set_aside_key(0)).unwrap(),
        Some(written_by_older)
      );
      assert_eq!(
        store.durable.get(Table::MlsStorage, LEGACY_KEY).unwrap(),
        Some(MOVED_MARKER.to_vec())
      );
      // A second one is set aside next to the first.
      store
        .put(Table::MlsStorage, LEGACY_KEY, &legacy(&[(b"other", b"x")]))
        .unwrap();
      load_mls_storage(&store).unwrap();
      assert!(
        store
          .durable
          .get(Table::MlsStorage, &set_aside_key(1))
          .unwrap()
          .is_some()
      );
      assert_eq!(durable_entries(&store), [entry(b"group", b"epoch 7")]);
    }

    /// Issue #424 review, H1: moved, then opened by an older version (which fails on the marker and
    /// writes nothing), then by this one again: nothing is lost.
    #[test]
    fn a_downgrade_and_back_loses_nothing() {
      let store = MemoryStore::new();
      store
        .put(
          Table::MlsStorage,
          LEGACY_KEY,
          &legacy(&[(b"a", b"1"), (b"b", b"2")]),
        )
        .unwrap();
      load_mls_storage(&store).unwrap();

      // The older version reads the old record only, and fails on it.
      let old_record = store.get(Table::MlsStorage, LEGACY_KEY).unwrap().unwrap();
      assert!(decode_legacy(&old_record).is_err());

      let (provider, _) = load_mls_storage(&store).unwrap();
      assert_eq!(entries_of(&provider), [entry(b"a", b"1"), entry(b"b", b"2")]);
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

    /// Issue #424 review, M1: a client loaded again on a store that lost writes (the engine reopens
    /// it after a panic) gets its baseline from a cache holding the lost group: its first save
    /// writes everything, the lost deletes included.
    #[test]
    fn a_client_loaded_after_a_lost_group_writes_everything() {
      let store = FlakyStore::default();
      let provider = provider_with(&[(b"X/context", b"epoch 1"), (b"X/secrets", b"epoch 1")]);
      let saved = SavedMlsStorage::default();
      save_mls_storage(&store, &provider, &saved).unwrap();
      set(&provider, b"X/context", Some(b"epoch 2"));
      set(&provider, b"X/secrets", Some(b"epoch 2"));
      set(&provider, b"X/proposal", Some(b"queued"));
      save_mls_storage(&store, &provider, &saved).unwrap();
      set(&provider, b"X/proposal", None);
      store.fail_next_group();
      assert!(write_group(&store, || save_mls_storage(&store, &provider, &saved)).is_err());

      let (reloaded, saved_again) = load_mls_storage(&store).unwrap();
      set(&reloaded, b"X/secrets", Some(b"epoch 2, one message later"));
      save_mls_storage(&store, &reloaded, &saved_again).unwrap();

      assert_eq!(durable_entries(&store), entries_of(&reloaded));
    }

    /// Issue #424 review, H2: another process committed to the store (the iOS notification
    /// extension, before it stopped writing MLS state). This process's state wins whole, as it did
    /// with one record, instead of mixing epochs per entry.
    #[test]
    fn after_another_writer_the_next_save_writes_everything() {
      let store = FlakyStore::default();
      let provider = provider_with(&[(b"X/context", b"epoch 1"), (b"X/secrets", b"epoch 1")]);
      let saved = SavedMlsStorage::default();
      save_mls_storage(&store, &provider, &saved).unwrap();
      store.write_elsewhere(b"X/context", b"epoch 2");
      store.write_elsewhere(b"X/secrets", b"epoch 2");

      set(&provider, b"X/secrets", Some(b"epoch 1, one message later"));
      store.take_counts();
      save_mls_storage(&store, &provider, &saved).unwrap();

      assert_eq!(store.take_counts(), (2, 0), "both entries written");
      assert_eq!(durable_entries(&store), entries_of(&provider));
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

    /// On a store without a tally (it keeps what it accepted, also from a save that failed) the
    /// failed save itself makes the next one write everything: an entry changed back since would
    /// otherwise look saved and stay as the failed save left it.
    #[test]
    fn after_a_failed_save_on_a_store_without_tally_the_next_one_writes_everything() {
      let store = FlakyStore {
        has_no_tally: true,
        ..FlakyStore::default()
      };
      let provider = provider_with(&[(b"a", b"1"), (b"b", b"2")]);
      let saved = SavedMlsStorage::default();
      save_mls_storage(&store, &provider, &saved).unwrap();

      set(&provider, b"a", Some(b"11"));
      store.fail_next_group();
      assert!(save_mls_storage(&store, &provider, &saved).is_err());
      assert_eq!(durable_entries(&store)[0], entry(b"a", b"11"), "kept anyway");
      set(&provider, b"a", Some(b"1"));
      store.take_counts();
      save_mls_storage(&store, &provider, &saved).unwrap();

      assert_eq!(store.take_counts(), (2, 0), "everything written");
      assert_eq!(durable_entries(&store), entries_of(&provider));
    }
  }

  /// What a save costs besides the writes, on a state like 300 groups' (docs/performance.md):
  /// `cargo test --release -p teamcraft-core --lib -- --ignored --nocapture measure_save`
  #[test]
  #[ignore = "measurement; prints the time of a save that changed one entry"]
  fn measure_save_of_one_change() {
    const GROUPS: usize = 300;
    const SAVES: u32 = 600;
    let provider = OpenMlsRustCrypto::default();
    for group in 0..GROUPS {
      // About 9 KB per group in a dozen entries, the tree the largest (as measured).
      for (entry, size) in [3_000, 2_000, 1_000, 800, 600, 500, 400, 300, 200, 100, 50, 50]
        .into_iter()
        .enumerate()
      {
        set(
          &provider,
          format!("{entry}/{group}").as_bytes(),
          Some(&vec![7; size]),
        );
      }
    }
    let store = MemoryStore::new();
    let saved = SavedMlsStorage::default();
    save_mls_storage(&store, &provider, &saved).unwrap();

    let started = std::time::Instant::now();
    for save in 0..SAVES {
      set(&provider, b"0/0", Some(&save.to_be_bytes()));
      save_mls_storage(&store, &provider, &saved).unwrap();
    }
    let per_save = started.elapsed() / SAVES;
    println!(
      "{} bytes in {} entries: {per_save:?} per save of one changed entry",
      mls_storage_len(&provider),
      GROUPS * 12
    );
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

  /// The entries on the fake store's disk (the marker and set-aside records left out).
  fn durable_entries(store: &FlakyStore) -> Vec<Entry> {
    let mut entries: Vec<Entry> = store
      .durable
      .list(Table::MlsStorage)
      .unwrap()
      .into_iter()
      .filter_map(|(key, value)| Some((parse_entry_key(&key)?, value)))
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
  /// disk rolls back and the tally counts; the deletes it lost are listed. With `has_no_tally` it
  /// is like the browser's instead: it reports the failure but keeps the group's writes. Counts
  /// the puts and deletes of MLS entries.
  #[derive(Default)]
  struct FlakyStore {
    cache: MemoryStore,
    durable: MemoryStore,
    has_no_tally: bool,
    state: Mutex<FlakyState>,
  }

  #[derive(Default)]
  struct FlakyState {
    depth: u32,
    group: Vec<(String, Option<Vec<u8>>)>,
    is_next_group_failing: bool,
    tally: WriteTally,
    lost_deletes: BTreeSet<String>,
    groups: usize,
    puts: usize,
    deletes: usize,
    marker_writes: usize,
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

    /// Another process commits an entry: on the disk, not in this store's cache.
    fn write_elsewhere(&self, storage_key: &[u8], value: &[u8]) {
      self
        .durable
        .put(Table::MlsStorage, &entry_key(storage_key), value)
        .unwrap();
      self.state.lock().unwrap().tally.foreign += 1;
    }

    /// Puts and deletes of MLS entries since the last call.
    fn take_counts(&self) -> (usize, usize) {
      let mut state = self.state.lock().unwrap();
      (
        std::mem::take(&mut state.puts),
        std::mem::take(&mut state.deletes),
      )
    }

    fn marker_writes(&self) -> usize {
      self.state.lock().unwrap().marker_writes
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
      if key.starts_with(ENTRY_PREFIX) {
        match maybe_value {
          Some(_) => state.puts += 1,
          None => state.deletes += 1,
        }
      } else if key == LEGACY_KEY && maybe_value == Some(MOVED_MARKER) {
        state.marker_writes += 1;
      }
      let write = (key.to_string(), maybe_value.map(<[u8]>::to_vec));
      if state.depth > 0 {
        state.group.push(write);
        Ok(())
      } else {
        state.lost_deletes.remove(key);
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
        if self.has_no_tally {
          for write in &group {
            apply(&self.durable, write)?;
          }
          return Err(StoreError::GroupFailed("made to fail, kept anyway".into()));
        }
        state.tally.lost += 1;
        for (key, maybe_value) in &group {
          if maybe_value.is_none() {
            state.lost_deletes.insert(key.clone());
          }
        }
        return Err(StoreError::GroupFailed("made to fail".into()));
      }
      for write in &group {
        state.lost_deletes.remove(&write.0);
        apply(&self.durable, write)?;
      }
      Ok(())
    }

    fn write_tally(&self) -> Option<WriteTally> {
      (!self.has_no_tally).then(|| self.state.lock().unwrap().tally)
    }

    fn lost_deletes(&self, table: Table) -> Vec<String> {
      assert_eq!(table, Table::MlsStorage);
      self.state.lock().unwrap().lost_deletes.iter().cloned().collect()
    }
  }
}
