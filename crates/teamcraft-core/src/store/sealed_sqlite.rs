//! [`LocalStore`] on plain SQLite with sealed records (e2ee.md §9.1), for platforms where SQLCipher
//! can't link a crypto library: Android (the NDK ships no libcrypto) and anywhere else that
//! enables the `sealed-sqlite` feature. The file holds only lookup IDs and ciphertext.
//!
//! The layout has a version (`PRAGMA user_version`, as in the SQLCipher store); a newer one is
//! refused (issue #160).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, params};
use teamcraft_crypto::local_store::{LOOKUP_ID_LEN, LookupId};

use super::sealed::{RecordBackend, SealedStore};
use super::sql_group::{WriteGroup, sqlite_error as database_error};
use super::{DatabaseKeySource, StoreError};

/// The layout this version writes; released 1.0 databases carry version 0 and this layout.
const SCHEMA_VERSION: i64 = 1;
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
    connection
      .execute_batch("PRAGMA journal_mode = WAL;")
      .map_err(database_error)?;
    migrate(&connection)?;
    let records = read_all(&connection)?;
    let backend = SqliteRecords {
      inner: Mutex::new(Inner {
        connection,
        group: WriteGroup::default(),
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
    let Inner { connection, group } = &mut *inner;
    group.begin(connection)
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let Inner { connection, group } = &mut *inner;
    group.end(connection)
  }
}

impl SqliteRecords {
  fn lock(&self) -> MutexGuard<'_, Inner> {
    // A poisoned lock only means another thread panicked; SQLite keeps the connection consistent.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Runs a write; a failure inside a group of writes undoes the whole group when it ends.
  fn write(&self, statement: impl FnOnce(&Connection) -> rusqlite::Result<usize>) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let result = statement(&inner.connection);
    result.map(|_| ()).map_err(|error| {
      inner.group.note_failure();
      database_error(error)
    })
  }
}

/// Brings the layout to [`SCHEMA_VERSION`] in one transaction; refuses a newer one.
fn migrate(connection: &Connection) -> Result<(), StoreError> {
  let found: i64 = connection
    .query_row("PRAGMA user_version", [], |row| row.get(0))
    .map_err(database_error)?;
  if found > SCHEMA_VERSION {
    return Err(StoreError::NewerSchema {
      found,
      supported: SCHEMA_VERSION,
    });
  }
  if found == SCHEMA_VERSION {
    return Ok(());
  }
  let transaction = connection.unchecked_transaction().map_err(database_error)?;
  if found < 1 {
    transaction.execute_batch(SCHEMA_V1).map_err(database_error)?;
  }
  // Forward-only: a later layout adds its step here and a fixture database (store/fixtures.rs).
  transaction
    .execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION}"))
    .map_err(database_error)?;
  transaction.commit().map_err(database_error)
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
    connection.execute_batch("PRAGMA user_version = 1").unwrap();
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
