//! [`LocalStore`] on plain SQLite with sealed records (e2ee.md §9.1), for platforms where SQLCipher
//! can't link a crypto library: Android (the NDK ships no libcrypto) and anywhere else that
//! enables the `sealed-sqlite` feature. The file holds only lookup IDs and ciphertext.
//!
//! The layout has a version (`PRAGMA user_version`, as in the SQLCipher store); a newer one is
//! refused (issue #160).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rusqlite::{Connection, params};
use teamcraft_crypto::local_store::{LOOKUP_ID_LEN, LookupId};

use super::sealed::{RecordBackend, SealedStore};
use super::sql_group::{
  Migration, MigrationKind, WriteGroup, run_migrations, sqlite_error as database_error,
};
use super::sqlite_wipe::wipe_free_pages;
use super::{DatabaseKeySource, StoreError, WriteTally};

/// The layout steps of this store, in order ([`run_migrations`]); released 1.0 databases carry
/// version 0 and the table of version 1.
pub(crate) const MIGRATIONS: &[Migration] = &[
  Migration {
    version: 1,
    kind: MigrationKind::Layout(|connection| connection.execute_batch(SCHEMA_V1).map_err(database_error)),
  },
  Migration {
    version: 2,
    kind: MigrationKind::Maintenance(wipe_free_pages),
  },
  // The MLS state is stored per entry (issue #424); versions before would read none of it.
  Migration {
    version: 3,
    kind: MigrationKind::Marker,
  },
];
/// How long a write waits for another connection's lock (a checkpoint of the log runs into one).
const BUSY_TIMEOUT: Duration = Duration::from_secs(1);
/// The most often a debounced purge ([`LocalStore::purge_freed_soon`]) truncates the log: a
/// checkpoint costs two or three syncs, and the MLS storage is saved for every processed message.
/// Old snapshots can stay in the log this long while the app runs (docs/e2ee.md §4.3.1).
const PURGE_INTERVAL: Duration = Duration::from_secs(30);
const SCHEMA_V1: &str = "
  CREATE TABLE IF NOT EXISTS sealed_records (
    id BLOB PRIMARY KEY,
    value BLOB NOT NULL
  ) WITHOUT ROWID;
";

/// The encrypted store on SQLite.
pub type SealedSqliteStore = SealedStore<SqliteRecords>;

/// The SQLite table holding the sealed records.
pub struct SqliteRecords {
  inner: Mutex<Inner>,
}

/// The connection and its open group of writes, under one lock.
struct Inner {
  connection: Connection,
  group: WriteGroup,
  purge: PurgeState,
}

/// What the write-ahead log still owes a purge.
struct PurgeState {
  /// Something was written since the log was last truncated, and a purge was asked for.
  is_due: bool,
  /// The caller wants it now (key deletion), not when the interval has passed.
  is_forced: bool,
  maybe_last_attempt: Option<Instant>,
  min_interval: Duration,
}

impl PurgeState {
  fn new() -> Self {
    Self {
      is_due: false,
      is_forced: false,
      maybe_last_attempt: None,
      min_interval: PURGE_INTERVAL,
    }
  }

  fn should_run(&self, now: Instant) -> bool {
    self.is_due
      && (self.is_forced
        || self
          .maybe_last_attempt
          .is_none_or(|last| now.saturating_duration_since(last) >= self.min_interval))
  }
}

impl SealedStore<SqliteRecords> {
  /// Opens (or creates) the database at `path` and decrypts it into memory.
  pub fn open(path: &Path, key_source: &impl DatabaseKeySource) -> Result<Self, StoreError> {
    Self::from_connection(Connection::open(path).map_err(database_error)?, key_source)
  }

  /// A database that lives only in memory, for tests.
  pub fn open_in_memory(key_source: &impl DatabaseKeySource) -> Result<Self, StoreError> {
    Self::from_connection(Connection::open_in_memory().map_err(database_error)?, key_source)
  }

  fn from_connection(
    connection: Connection,
    key_source: &impl DatabaseKeySource,
  ) -> Result<Self, StoreError> {
    // secure_delete: freed pages are zeroed, so a deleted record's ciphertext doesn't linger in
    // them (issue #391); the write-ahead log is cleared by `purge_freed`.
    connection
      .execute_batch("PRAGMA journal_mode = WAL; PRAGMA secure_delete = ON;")
      .map_err(database_error)?;
    connection.busy_timeout(BUSY_TIMEOUT).map_err(database_error)?;
    run_migrations(&connection, MIGRATIONS)?;
    // Before the read: a commit of another connection after it counts (the cache doesn't hold it).
    let mut group = WriteGroup::default();
    group.watch_other_connections(&connection)?;
    let records = read_all(&connection)?;
    let backend = SqliteRecords {
      inner: Mutex::new(Inner {
        connection,
        group,
        purge: PurgeState::new(),
      }),
    };
    Self::load(backend, &key_source.database_key()?, records)
  }
}

impl RecordBackend for SqliteRecords {
  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    self.write(|connection| {
      connection.execute(
        "INSERT INTO sealed_records (id, value) VALUES (?1, ?2)
         ON CONFLICT (id) DO UPDATE SET value = excluded.value",
        params![id.0.as_slice(), sealed],
      )
    })
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    self.write(|connection| {
      connection.execute(
        "DELETE FROM sealed_records WHERE id = ?1",
        params![id.0.as_slice()],
      )
    })
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let Inner {
      connection, group, ..
    } = &mut *inner;
    group.begin(connection)
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let Inner {
      connection,
      group,
      purge,
    } = &mut *inner;
    group.end(connection)?;
    // The group is committed whatever happens to the purge: a failed one stays due and must not
    // make the committed writes look failed.
    if !group.is_open() {
      run_purge_if_due(connection, purge);
    }
    Ok(())
  }

  /// Writes the log back into the database (whose freed pages `secure_delete` zeroes) and
  /// truncates it, so no old page version stays in the log file. Inside a group of writes it waits
  /// for the group's commit: a checkpoint before would write half of the group to the database.
  fn purge_freed(&self) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let Inner {
      connection,
      group,
      purge,
    } = &mut *inner;
    purge.is_due = true;
    purge.is_forced = true;
    if !group.is_open() {
      run_purge_if_due(connection, purge);
    }
    Ok(())
  }

  fn purge_freed_soon(&self) {
    let mut inner = self.lock();
    let Inner {
      connection,
      group,
      purge,
    } = &mut *inner;
    purge.is_due = true;
    if !group.is_open() {
      run_purge_if_due(connection, purge);
    }
  }

  fn write_tally(&self) -> Option<WriteTally> {
    let mut inner = self.lock();
    let Inner {
      connection, group, ..
    } = &mut *inner;
    Some(group.tally(connection))
  }
}

impl Drop for SqliteRecords {
  fn drop(&mut self) {
    // Closing the app is the last chance to clear what the interval kept back.
    let mut inner = self.lock();
    let Inner {
      connection, purge, ..
    } = &mut *inner;
    if purge.is_due {
      purge.is_forced = true;
      run_purge_if_due(connection, purge);
    }
  }
}

/// Truncates the log if a purge is due and its time has come. A checkpoint that did not complete
/// (another connection holds the log, the disk failed) leaves the purge due; it is retried by the
/// next write, purge or drop, never reported as a failure of the write that asked for it.
fn run_purge_if_due(connection: &Connection, purge: &mut PurgeState) {
  let now = Instant::now();
  if !purge.should_run(now) {
    return;
  }
  purge.maybe_last_attempt = Some(now);
  match checkpoint_truncate(connection) {
    Ok(true) => {
      purge.is_due = false;
      purge.is_forced = false;
    },
    Ok(false) => tracing::warn!("the write-ahead log could not be truncated yet; trying again later"),
    Err(error) => tracing::warn!(%error, "could not truncate the write-ahead log; trying again later"),
  }
}

/// `PRAGMA wal_checkpoint(TRUNCATE)`; `true` only if it ran to the end: not blocked by a reader
/// or writer (`busy`) and every frame of the log written back.
fn checkpoint_truncate(connection: &Connection) -> Result<bool, StoreError> {
  let (busy, log_frames, checkpointed_frames): (i64, i64, i64) = connection
    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
      Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })
    .map_err(database_error)?;
  // Without a log (a database that is not in WAL mode) SQLite answers -1 for both counts.
  Ok(busy == 0 && log_frames == checkpointed_frames)
}

impl SqliteRecords {
  fn lock(&self) -> MutexGuard<'_, Inner> {
    // A poisoned lock only means another thread panicked; SQLite keeps the connection consistent.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Runs a write; a failure inside a group of writes undoes the whole group when it ends.
  fn write(&self, statement: impl FnOnce(&Connection) -> rusqlite::Result<usize>) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let Inner {
      connection, group, ..
    } = &mut *inner;
    group.check_open(connection)?;
    let result = statement(&inner.connection);
    result.map(|_| ()).map_err(|error| {
      inner.group.note_failure();
      database_error(error)
    })
  }
}

fn read_all(connection: &Connection) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
  let mut statement = connection
    .prepare("SELECT id, value FROM sealed_records")
    .map_err(database_error)?;
  let rows = statement
    .query_map([], |row| {
      Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?))
    })
    .map_err(database_error)?;
  rows
    .map(|row| {
      let (id, value) = row.map_err(database_error)?;
      let id = <[u8; LOOKUP_ID_LEN]>::try_from(id.as_slice())
        .map_err(|_| StoreError::Corrupt("record ID length".into()))?;
      Ok((LookupId(id), value))
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::secret::SymmetricKey;

  use super::*;
  use crate::store::tests::exercise_store;
  use crate::store::{LocalStore, StaticKeySource, Table};

  #[test]
  fn sealed_sqlite_behaves_like_a_store() {
    let key = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
    exercise_store(&SealedSqliteStore::open_in_memory(&key).unwrap());
  }

  #[test]
  fn file_reopens_with_the_key_and_holds_no_plaintext() {
    let directory = std::env::temp_dir().join(format!(
      "teamcraft-sealed-{}",
      rand_core::RngCore::next_u64(&mut OsRng)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    {
      let store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      store
        .put(Table::Users, "1", b"very recognizable plaintext")
        .unwrap();
    }
    let raw = std::fs::read(&path).unwrap();
    let wal = std::fs::read(path.with_extension("db-wal")).unwrap_or_default();
    for bytes in [&raw, &wal] {
      assert!(!bytes.windows(17).any(|window| window == b"very recognizable"));
      assert!(!bytes.windows(5).any(|window| window == b"users"));
    }
    let wrong = SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng)));
    assert!(matches!(wrong, Err(StoreError::WrongKey)));
    let reopened = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    assert_eq!(
      reopened.get(Table::Users, "1").unwrap(),
      Some(b"very recognizable plaintext".to_vec())
    );
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #391: the old copy of a deleted record (a key package's private part in the MLS
  /// storage) is neither in a freed page nor in the write-ahead log once the store purged.
  #[test]
  fn a_deleted_record_leaves_no_ciphertext_in_the_files() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store =
      SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    store.put(Table::MlsStorage, "state", &[7; 4096]).unwrap();
    let sealed: Vec<u8> = Connection::open(&path)
      .unwrap()
      .query_row("SELECT value FROM sealed_records", [], |row| row.get(0))
      .unwrap();
    let needle = &sealed[sealed.len() / 2..sealed.len() / 2 + 32];

    store.delete(Table::MlsStorage, "state").unwrap();
    store.purge_freed().unwrap();

    let raw = std::fs::read(&path).unwrap();
    let wal = std::fs::read(path.with_extension("db-wal")).unwrap_or_default();
    for bytes in [&raw, &wal] {
      assert!(!bytes.windows(needle.len()).any(|window| window == needle));
    }
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #417: saving the MLS storage inside a group of writes clears the old snapshot from the
  /// log once the group is committed.
  #[test]
  fn a_purge_inside_a_group_waits_for_the_commit() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store =
      SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    store.put(Table::MlsStorage, "state", &[7; 4096]).unwrap();
    let sealed: Vec<u8> = Connection::open(&path)
      .unwrap()
      .query_row("SELECT value FROM sealed_records", [], |row| row.get(0))
      .unwrap();
    let needle = &sealed[sealed.len() / 2..sealed.len() / 2 + 32];

    store.begin_writes().unwrap();
    store.put(Table::MlsStorage, "state", &[9; 4096]).unwrap();
    store.purge_freed().unwrap();
    store.end_writes().unwrap();

    let raw = std::fs::read(&path).unwrap();
    let wal = std::fs::read(path.with_extension("db-wal")).unwrap_or_default();
    for bytes in [&raw, &wal] {
      assert!(!bytes.windows(needle.len()).any(|window| window == needle));
    }
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  fn is_purge_due(store: &SealedSqliteStore) -> bool {
    store.backend().lock().purge.is_due
  }

  /// Issue #417: the MLS storage is saved for every message, so those purges are merged; an
  /// explicit purge, and closing the store, still clear the log.
  #[test]
  fn debounced_purges_are_merged_and_an_explicit_one_clears_what_is_left() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store =
      SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    store.put(Table::MlsStorage, "state", &[1; 64]).unwrap();
    store.purge_freed_soon();
    assert!(!is_purge_due(&store), "the first one runs at once");
    store.put(Table::MlsStorage, "state", &[2; 64]).unwrap();
    store.purge_freed_soon();
    assert!(is_purge_due(&store), "the next one within the interval waits");
    store.purge_freed().unwrap();
    assert!(!is_purge_due(&store));
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn closing_the_store_clears_the_log_of_a_waiting_purge() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store =
      SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    store.put(Table::MlsStorage, "state", &[7; 4096]).unwrap();
    let sealed: Vec<u8> = Connection::open(&path)
      .unwrap()
      .query_row("SELECT value FROM sealed_records", [], |row| row.get(0))
      .unwrap();
    let needle = sealed[sealed.len() / 2..sealed.len() / 2 + 32].to_vec();
    store.purge_freed_soon();
    store.put(Table::MlsStorage, "state", &[9; 4096]).unwrap();
    store.purge_freed_soon();
    assert!(is_purge_due(&store));
    drop(store);
    let raw = std::fs::read(&path).unwrap();
    let wal = std::fs::read(path.with_extension("db-wal")).unwrap_or_default();
    for bytes in [&raw, &wal] {
      assert!(!bytes.windows(needle.len()).any(|window| window == needle));
    }
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// A reader that holds the log keeps the checkpoint from finishing; the purge stays due and the
  /// write that asked for it is not reported as failed.
  #[test]
  fn a_checkpoint_that_cannot_finish_stays_due_without_failing_the_write() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store =
      SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    store.put(Table::MlsStorage, "state", &[1; 64]).unwrap();
    // A second connection in the middle of a read transaction pins the log.
    let reader = Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    reader
      .query_row("SELECT count(*) FROM sealed_records", [], |row| {
        row.get::<_, i64>(0)
      })
      .unwrap();
    store.put(Table::MlsStorage, "state", &[2; 64]).unwrap();
    store.purge_freed().unwrap();
    assert!(is_purge_due(&store), "an unfinished checkpoint is retried");
    reader.execute_batch("COMMIT").unwrap();
    store.purge_freed().unwrap();
    assert!(!is_purge_due(&store));
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn migration_steps_are_numbered() {
    crate::store::sql_group::tests::assert_numbered(MIGRATIONS);
  }

  #[test]
  fn an_interrupted_group_of_writes_leaves_none_of_them() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    {
      let store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      store.put(Table::Users, "before", b"kept").unwrap();
      store.begin_writes().unwrap();
      store.put(Table::Outbox, "1", b"entry").unwrap();
      store.put(Table::SyncState, "sync", b"cursor").unwrap();
    }
    let reopened = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    assert_eq!(
      reopened.get(Table::Users, "before").unwrap(),
      Some(b"kept".to_vec())
    );
    assert_eq!(reopened.get(Table::Outbox, "1").unwrap(), None);
    assert_eq!(reopened.get(Table::SyncState, "sync").unwrap(), None);
    drop(reopened);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #424: the cache keeps a group the database rolled back, a deleted MLS entry included,
  /// so the store's list no longer shows what the disk still holds. The tally makes the next
  /// save write the whole state and delete that entry again: after a restart the database holds
  /// what the provider holds.
  #[test]
  fn the_mls_state_is_written_again_after_a_rolled_back_group() {
    use openmls_traits::OpenMlsProvider as _;

    use crate::mls_storage::{SavedMlsStorage, load_mls_storage, save_mls_storage};

    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
    let set = |key: &[u8], maybe_value: Option<&[u8]>| {
      let mut values = provider.storage().values.write().unwrap();
      match maybe_value {
        Some(value) => values.insert(key.to_vec(), value.to_vec()),
        None => values.remove(key),
      };
    };
    {
      let store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      let saved = SavedMlsStorage::default();
      set(b"epoch", Some(b"1"));
      set(b"spent key package", Some(b"secret"));
      save_mls_storage(&store, &provider, &saved).unwrap();

      set(b"epoch", Some(b"2"));
      set(b"spent key package", None);
      store.begin_writes().unwrap();
      save_mls_storage(&store, &provider, &saved).unwrap();
      store
        .backend()
        .lock()
        .connection
        .execute_batch("ROLLBACK")
        .unwrap();
      assert!(store.end_writes().is_err());
      assert_eq!(store.write_tally(), Some(WriteTally { lost: 1, foreign: 0 }));
      assert_eq!(
        store.lost_deletes(Table::MlsStorage).len(),
        1,
        "the cache dropped the spent key package, the disk still has it"
      );

      set(b"next group", Some(b"3"));
      save_mls_storage(&store, &provider, &saved).unwrap();
    }
    let reopened = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    let (loaded, _) = load_mls_storage(&reopened).unwrap();
    assert_eq!(
      *loaded.storage().values.read().unwrap(),
      *provider.storage().values.read().unwrap()
    );
    drop(reopened);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #424 review, M1 (the reviewer's demo): the engine reopens the client on the same store
  /// after a panic in the sync loop. Its baseline comes from the cache, which holds the lost
  /// group; the first save after the reopen writes everything, the lost delete included.
  #[test]
  fn a_client_reopened_after_a_lost_group_leaves_no_mix_on_disk() {
    use openmls_traits::OpenMlsProvider as _;

    use crate::mls_storage::{SavedMlsStorage, load_mls_storage, save_mls_storage};

    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let set = |provider: &openmls_rust_crypto::OpenMlsRustCrypto, key: &[u8], maybe_value: Option<&[u8]>| {
      let mut values = provider.storage().values.write().unwrap();
      match maybe_value {
        Some(value) => values.insert(key.to_vec(), value.to_vec()),
        None => values.remove(key),
      };
    };
    let expected;
    {
      let store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      let provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
      let saved = SavedMlsStorage::default();
      set(&provider, b"X/context", Some(b"epoch 1"));
      set(&provider, b"X/secrets", Some(b"epoch 1"));
      set(&provider, b"X/proposal", Some(b"queued"));
      save_mls_storage(&store, &provider, &saved).unwrap();

      // A commit in group X; the outer group is rolled back (a full disk).
      set(&provider, b"X/context", Some(b"epoch 2"));
      set(&provider, b"X/secrets", Some(b"epoch 2"));
      set(&provider, b"X/proposal", None);
      store.begin_writes().unwrap();
      save_mls_storage(&store, &provider, &saved).unwrap();
      store
        .backend()
        .lock()
        .connection
        .execute_batch("ROLLBACK")
        .unwrap();
      assert!(store.end_writes().is_err());

      // Reopened on the same store; the next message in X changes only the secrets.
      let (reopened, saved_again) = load_mls_storage(&store).unwrap();
      set(&reopened, b"X/secrets", Some(b"epoch 2, one message later"));
      save_mls_storage(&store, &reopened, &saved_again).unwrap();
      expected = reopened.storage().values.read().unwrap().clone();
    }
    let from_disk = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    let (loaded, _) = load_mls_storage(&from_disk).unwrap();
    let on_disk = loaded.storage().values.read().unwrap().clone();
    drop(from_disk);
    std::fs::remove_dir_all(&directory).unwrap();
    assert_eq!(on_disk, expected, "the disk holds what the reopened client holds");
  }

  /// Issue #424 review, H2 (the reviewer's demo): two processes on one database, the long-lived
  /// one with a cache older than the other's commit. Its next save writes its whole state, as the
  /// one record did, instead of mixing the other's epoch 2 context with its epoch 1 secrets.
  #[test]
  fn a_writer_with_an_older_cache_never_mixes_epochs_per_entry() {
    use openmls_traits::OpenMlsProvider as _;

    use crate::mls_storage::{SavedMlsStorage, load_mls_storage, save_mls_storage};

    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let set = |provider: &openmls_rust_crypto::OpenMlsRustCrypto, key: &[u8], value: &[u8]| {
      provider
        .storage()
        .values
        .write()
        .unwrap()
        .insert(key.to_vec(), value.to_vec());
    };
    {
      let first = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      let provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
      set(&provider, b"X/context", b"epoch 1");
      set(&provider, b"X/secrets", b"epoch 1");
      save_mls_storage(&first, &provider, &SavedMlsStorage::default()).unwrap();
    }
    let app_store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
    let (app, app_saved) = load_mls_storage(&app_store).unwrap();
    {
      let other_store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      let (other, other_saved) = load_mls_storage(&other_store).unwrap();
      set(&other, b"X/context", b"epoch 2");
      set(&other, b"X/secrets", b"epoch 2");
      save_mls_storage(&other_store, &other, &other_saved).unwrap();
    }
    set(&app, b"X/secrets", b"epoch 1, one message later");
    save_mls_storage(&app_store, &app, &app_saved).unwrap();
    drop(app_store);

    let reopened = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    let (loaded, _) = load_mls_storage(&reopened).unwrap();
    let values = loaded.storage().values.read().unwrap().clone();
    drop(reopened);
    std::fs::remove_dir_all(&directory).unwrap();
    assert_eq!(
      (
        values[b"X/context".as_slice()].as_slice(),
        values[b"X/secrets".as_slice()].as_slice()
      ),
      (b"epoch 1".as_slice(), b"epoch 1, one message later".as_slice()),
      "one writer's whole state, not mixed epochs"
    );
  }

  /// Issue #424 review, H1: 1.0.8 (its steps end before the marker) refuses the database.
  #[test]
  fn a_version_before_the_mls_state_per_entry_refuses_the_database() {
    let store =
      SealedSqliteStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    let as_in_1_0_8 = &MIGRATIONS[..2];
    assert!(matches!(
      run_migrations(&store.backend().lock().connection, as_in_1_0_8),
      Err(StoreError::NewerSchema {
        found: 3,
        supported: 2
      })
    ));
  }

  /// Lost deletes are forgotten once a later group that reached the disk deleted or wrote them.
  #[test]
  fn a_lost_delete_is_forgotten_once_written_again() {
    let store =
      SealedSqliteStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    store.put(Table::MlsStorage, "entry/a", b"1").unwrap();
    store.begin_writes().unwrap();
    store.delete(Table::MlsStorage, "entry/a").unwrap();
    store
      .backend()
      .lock()
      .connection
      .execute_batch("ROLLBACK")
      .unwrap();
    assert!(store.end_writes().is_err());
    assert_eq!(store.lost_deletes(Table::MlsStorage), ["entry/a"]);
    assert!(store.lost_deletes(Table::Users).is_empty());

    crate::store::write_group(&store, || store.delete(Table::MlsStorage, "entry/a")).unwrap();
    assert!(store.lost_deletes(Table::MlsStorage).is_empty());
  }

  #[test]
  fn a_newer_layout_is_refused_and_damaged_records_are_reported() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    {
      let store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      store.put(Table::Users, "1", b"one").unwrap();
      store.put(Table::Users, "2", b"two").unwrap();
    }
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA user_version = 99").unwrap();
    let newer = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate()));
    assert!(matches!(newer, Err(StoreError::NewerSchema { found: 99, .. })));
    connection
      .execute_batch(&format!(
        "PRAGMA user_version = {}",
        crate::store::sql_group::latest_version(MIGRATIONS)
      ))
      .unwrap();
    // One record damaged, the others fine: not a wrong key but a damaged database.
    connection
      .execute(
        "UPDATE sealed_records SET value = x'00' WHERE id = (SELECT id FROM sealed_records LIMIT 1)",
        [],
      )
      .unwrap();
    drop(connection);
    let damaged = SealedSqliteStore::open(&path, &StaticKeySource::new(key));
    assert!(matches!(damaged, Err(StoreError::Corrupt(_))), "{damaged:?}");
    std::fs::remove_dir_all(&directory).unwrap();
  }

  fn scratch_dir() -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
      "teamcraft-sealed-{}",
      rand_core::RngCore::next_u64(&mut OsRng)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
  }
}
