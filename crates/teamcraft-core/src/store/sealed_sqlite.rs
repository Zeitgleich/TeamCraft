//! [`LocalStore`] on plain SQLite with sealed records (e2ee.md §9.1), for platforms where SQLCipher
//! can't link a crypto library: Android (the NDK ships no libcrypto) and anywhere else that
//! enables the `sealed-sqlite` feature. The file holds only lookup IDs and ciphertext.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, params};
use teamcraft_crypto::local_store::{LOOKUP_ID_LEN, LookupId};

use super::sealed::{RecordBackend, SealedStore};
use super::{DatabaseKeySource, StoreError};

const SCHEMA: &str = "
  CREATE TABLE IF NOT EXISTS sealed_records (
    id BLOB PRIMARY KEY,
    value BLOB NOT NULL
  ) WITHOUT ROWID;
";

/// The encrypted store on SQLite.
pub type SealedSqliteStore = SealedStore<SqliteRecords>;

/// The SQLite table holding the sealed records.
pub struct SqliteRecords {
  connection: Mutex<Connection>,
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
    connection.execute_batch(SCHEMA).map_err(database_error)?;
    let records = read_all(&connection)?;
    let backend = SqliteRecords {
      connection: Mutex::new(connection),
    };
    Self::load(backend, &key_source.database_key()?, records)
  }
}

impl RecordBackend for SqliteRecords {
  fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
    self
      .lock()
      .execute(
        "INSERT INTO sealed_records (id, value) VALUES (?1, ?2)
         ON CONFLICT (id) DO UPDATE SET value = excluded.value",
        params![id.0.as_slice(), sealed],
      )
      .map_err(database_error)?;
    Ok(())
  }

  fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
    self
      .lock()
      .execute(
        "DELETE FROM sealed_records WHERE id = ?1",
        params![id.0.as_slice()],
      )
      .map_err(database_error)?;
    Ok(())
  }

  /// Writes the log back into the database (whose freed pages `secure_delete` zeroes) and
  /// truncates it, so no old page version stays in the log file.
  fn purge_freed(&self) -> Result<(), StoreError> {
    self
      .lock()
      .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
      .map_err(database_error)
  }
}

impl SqliteRecords {
  fn lock(&self) -> MutexGuard<'_, Connection> {
    // A poisoned lock only means another thread panicked; SQLite keeps the connection consistent.
    self
      .connection
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner())
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

fn database_error(error: rusqlite::Error) -> StoreError {
  StoreError::Database(error.to_string())
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
    let directory = std::env::temp_dir().join(format!(
      "teamcraft-sealed-{}",
      rand_core::RngCore::next_u64(&mut OsRng)
    ));
    std::fs::create_dir_all(&directory).unwrap();
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
}
