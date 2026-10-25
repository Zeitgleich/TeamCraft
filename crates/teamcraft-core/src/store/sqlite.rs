//! Native [`LocalStore`] on SQLCipher (e2ee.md §9). The whole file is encrypted with a raw 256-bit
//! key from a [`DatabaseKeySource`]; nothing readable touches the disk.
//!
//! The layout has a version (`PRAGMA user_version`, [`MIGRATIONS`]); opening upgrades an older
//! database step by step and refuses a newer one instead of misreading it (issue #160).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use teamcraft_proto::{ClientMessageId, ConversationId, Id, MessageId};
use zeroize::Zeroizing;

use super::sql_group::{
  Migration, MigrationKind, WriteGroup, run_migrations, sqlite_error as database_error,
};
use super::sqlite_wipe::wipe_free_pages;
use super::{DatabaseKeySource, LocalStore, MessageFilter, StoreError, StoredMessage, Table};

/// The layout steps of this store, in order ([`run_migrations`]). Released 1.0 databases carry
/// version 0 (no version was set) and the tables of version 1, apart from databases older than the
/// `in_channel` column; step 1 is written to accept both.
pub(crate) const MIGRATIONS: &[Migration] = &[
  Migration {
    version: 1,
    kind: MigrationKind::Layout(migrate_to_v1),
  },
  Migration {
    version: 2,
    kind: MigrationKind::Maintenance(wipe_free_pages),
  },
];
/// The layout this version writes.
#[cfg(test)]
const SCHEMA_VERSION: i64 = super::sql_group::latest_version(MIGRATIONS);

/// The tables of version 1.
const SCHEMA_V1: &str = "
  CREATE TABLE IF NOT EXISTS kv (
    tbl TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    PRIMARY KEY (tbl, key)
  );
  CREATE TABLE IF NOT EXISTS messages (
    conversation_id INTEGER NOT NULL,
    client_message_id BLOB NOT NULL,
    sort_group INTEGER NOT NULL,
    sort_value INTEGER NOT NULL,
    thread_id INTEGER,
    is_hidden INTEGER NOT NULL,
    record BLOB NOT NULL,
    PRIMARY KEY (conversation_id, client_message_id)
  );
  CREATE INDEX IF NOT EXISTS messages_order ON messages (conversation_id, sort_group, sort_value);
";

pub struct SqlCipherStore {
  inner: Mutex<Inner>,
}

/// The connection and the group of writes open on it: one lock, so a write and the group it joins
/// never disagree.
struct Inner {
  connection: Connection,
  group: WriteGroup,
}

impl SqlCipherStore {
  /// Opens (or creates) the database at `path`, keyed by `key_source`.
  pub fn open(path: &Path, key_source: &impl DatabaseKeySource) -> Result<Self, StoreError> {
    let connection = Connection::open(path).map_err(database_error)?;
    Self::initialize(connection, key_source)
  }

  /// An encrypted database that lives only in memory, for tests.
  pub fn open_in_memory(key_source: &impl DatabaseKeySource) -> Result<Self, StoreError> {
    Self::initialize(Connection::open_in_memory().map_err(database_error)?, key_source)
  }

  fn initialize(connection: Connection, key_source: &impl DatabaseKeySource) -> Result<Self, StoreError> {
    let key = key_source.database_key()?;
    let hex: Zeroizing<String> = Zeroizing::new(
      key
        .expose_secret()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect(),
    );
    // Raw key syntax: SQLCipher uses the 256 bits directly instead of deriving them from a passphrase.
    let pragma = Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", hex.as_str()));
    connection.execute_batch(&pragma).map_err(database_error)?;
    // The key is only checked on first access; a wrong one shows up as "not a database".
    connection
      .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
      })
      .map_err(|_| StoreError::WrongKey)?;
    // Freed pages are zeroed (before encryption), so an old copy of a rewritten or deleted record
    // doesn't stay readable to whoever later gets the key (issue #391). The rollback journal holds
    // old pages during a transaction; it is unlinked afterwards, not overwritten, so its blocks may
    // linger on disk until reused (encrypted with the database key).
    connection
      .execute_batch("PRAGMA secure_delete = ON;")
      .map_err(database_error)?;
    run_migrations(&connection, MIGRATIONS)?;
    Ok(Self {
      inner: Mutex::new(Inner {
        connection,
        group: WriteGroup::default(),
      }),
    })
  }

  fn lock(&self) -> MutexGuard<'_, Inner> {
    // A poisoned lock only means another thread panicked; SQLite keeps the connection consistent.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Runs a write; a failure inside a group of writes undoes the whole group when it ends.
  fn write(&self, statement: impl FnOnce(&Connection) -> rusqlite::Result<usize>) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let Inner { connection, group } = &mut *inner;
    group.check_open(connection)?;
    let result = statement(&inner.connection);
    match result {
      Ok(_) => Ok(()),
      Err(error) => {
        inner.group.note_failure();
        Err(database_error(error))
      },
    }
  }
}

impl LocalStore for SqlCipherStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    self
      .lock()
      .connection
      .query_row(
        "SELECT value FROM kv WHERE tbl = ?1 AND key = ?2",
        params![table.name(), key],
        |row| row.get(0),
      )
      .optional()
      .map_err(database_error)
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    self.write(|connection| {
      connection.execute(
        "INSERT INTO kv (tbl, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (tbl, key) DO UPDATE SET value = excluded.value",
        params![table.name(), key, value],
      )
    })
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self.write(|connection| {
      connection.execute(
        "DELETE FROM kv WHERE tbl = ?1 AND key = ?2",
        params![table.name(), key],
      )
    })
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let inner = self.lock();
    let mut statement = inner
      .connection
      .prepare("SELECT key, value FROM kv WHERE tbl = ?1 ORDER BY key")
      .map_err(database_error)?;
    let rows = statement
      .query_map(params![table.name()], |row| Ok((row.get(0)?, row.get(1)?)))
      .map_err(database_error)?;
    rows.collect::<Result<_, _>>().map_err(database_error)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    let record = serde_json::to_vec(message).map_err(|error| StoreError::Corrupt(error.to_string()))?;
    let (sort_group, sort_value) = message.sort_key();
    self.write(|connection| {
      connection.execute(
        "INSERT INTO messages
           (conversation_id, client_message_id, sort_group, sort_value, thread_id, is_hidden, in_channel, record)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT (conversation_id, client_message_id) DO UPDATE SET
           sort_group = excluded.sort_group, sort_value = excluded.sort_value,
           thread_id = excluded.thread_id, is_hidden = excluded.is_hidden,
           in_channel = excluded.in_channel, record = excluded.record",
        params![
          message.conversation_id.snowflake().get(),
          message.client_message_id.as_bytes().as_slice(),
          sort_group,
          sort_value,
          message.maybe_thread_id.map(|thread| thread.snowflake().get()),
          message.is_hidden(),
          message.is_also_in_channel(),
          record,
        ],
      )
    })
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    let maybe_record: Option<Vec<u8>> = self
      .lock()
      .connection
      .query_row(
        "SELECT record FROM messages WHERE conversation_id = ?1 AND client_message_id = ?2",
        params![
          conversation.snowflake().get(),
          client_message_id.as_bytes().as_slice()
        ],
        |row| row.get(0),
      )
      .optional()
      .map_err(database_error)?;
    maybe_record.map(|record| decode_message(&record)).transpose()
  }

  /// Server messages sort by their ID (`sort_group` 0), so the order index finds one directly.
  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    let maybe_record: Option<Vec<u8>> = self
      .lock()
      .connection
      .query_row(
        "SELECT record FROM messages WHERE conversation_id = ?1 AND sort_group = 0 AND sort_value = ?2",
        params![conversation.snowflake().get(), server_id.snowflake().get()],
        |row| row.get(0),
      )
      .optional()
      .map_err(database_error)?;
    maybe_record.map(|record| decode_message(&record)).transpose()
  }

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let inner = self.lock();
    let (condition, maybe_thread) = match filter {
      MessageFilter::All => ("", None),
      MessageFilter::TopLevel => (
        "AND (thread_id IS NULL OR in_channel = 1) AND is_hidden = 0",
        None,
      ),
      MessageFilter::Thread(thread) => (
        "AND thread_id = ?2 AND is_hidden = 0",
        Some(thread.snowflake().get()),
      ),
    };
    let sql = format!(
      "SELECT record FROM messages WHERE conversation_id = ?1 {condition} ORDER BY sort_group, sort_value"
    );
    let mut statement = inner.connection.prepare(&sql).map_err(database_error)?;
    let mut values = vec![conversation.snowflake().get()];
    values.extend(maybe_thread);
    let records: Vec<Vec<u8>> = statement
      .query_map(params_from_iter(values), |row| row.get(0))
      .map_err(database_error)?
      .collect::<Result<_, _>>()
      .map_err(database_error)?;
    records.iter().map(|record| decode_message(record)).collect()
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    self.write(|connection| {
      connection.execute(
        "DELETE FROM messages WHERE conversation_id = ?1",
        params![conversation.snowflake().get()],
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

impl std::fmt::Debug for SqlCipherStore {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("SqlCipherStore")
  }
}

fn decode_message(record: &[u8]) -> Result<StoredMessage, StoreError> {
  serde_json::from_slice(record).map_err(|error| StoreError::Corrupt(error.to_string()))
}

/// Version 1: the tables, and the `in_channel` column that early 1.0 previews lacked (thread
/// replies also sent to the conversation, `MessageMeta::is_also_in_channel`).
fn migrate_to_v1(connection: &Connection) -> Result<(), StoreError> {
  connection.execute_batch(SCHEMA_V1).map_err(database_error)?;
  let has_in_channel = connection
    .prepare("SELECT 1 FROM pragma_table_info('messages') WHERE name = 'in_channel'")
    .and_then(|mut statement| statement.exists([]))
    .map_err(database_error)?;
  if !has_in_channel {
    connection
      .execute_batch("ALTER TABLE messages ADD COLUMN in_channel INTEGER NOT NULL DEFAULT 0")
      .map_err(database_error)?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::secret::SymmetricKey;

  use super::*;
  use crate::store::StaticKeySource;
  use crate::store::tests::exercise_store;

  /// Issue #391: freed pages are zeroed, so a deleted record's old copy doesn't stay in the file.
  #[test]
  fn freed_pages_are_overwritten() {
    let key = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
    let store = SqlCipherStore::open_in_memory(&key).unwrap();
    let secure_delete: i64 = store
      .lock()
      .connection
      .query_row("PRAGMA secure_delete", [], |row| row.get(0))
      .unwrap();
    assert_eq!(secure_delete, 1);
  }

  #[test]
  fn sqlcipher_store_behaves_like_a_store() {
    let store =
      SqlCipherStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    exercise_store(&store);
  }

  #[test]
  fn file_is_encrypted_and_needs_the_right_key() {
    let directory = std::env::temp_dir().join(format!("teamcraft-core-test-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("store.db");
    let _ = std::fs::remove_file(&path);
    let key = SymmetricKey::generate(&mut OsRng);
    {
      let store = SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      store
        .put(Table::Users, "1", b"very recognizable plaintext")
        .unwrap();
    }
    let raw = std::fs::read(&path).unwrap();
    assert!(!raw.windows(12).any(|window| window == b"recognizable"));
    assert!(!raw.starts_with(b"SQLite format 3"));

    let wrong = SqlCipherStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng)));
    assert!(matches!(wrong, Err(StoreError::WrongKey)));
    let reopened = SqlCipherStore::open(&path, &StaticKeySource::new(key)).unwrap();
    assert_eq!(
      reopened.get(Table::Users, "1").unwrap(),
      Some(b"very recognizable plaintext".to_vec())
    );
    // Windows can't delete a database file that is still open.
    drop(reopened);
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
      let store = SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      store.put(Table::Users, "before", b"kept").unwrap();
      store.begin_writes().unwrap();
      store.put(Table::Outbox, "1", b"entry").unwrap();
      store.put(Table::SyncState, "sync", b"cursor").unwrap();
      // The app dies here: the group never ends.
    }
    let reopened = SqlCipherStore::open(&path, &StaticKeySource::new(key)).unwrap();
    assert_eq!(
      reopened.get(Table::Users, "before").unwrap(),
      Some(b"kept".to_vec())
    );
    assert_eq!(reopened.get(Table::Outbox, "1").unwrap(), None);
    assert_eq!(reopened.get(Table::SyncState, "sync").unwrap(), None);
    // And the reopened store commits groups again.
    crate::store::write_group(&reopened, || reopened.put(Table::Outbox, "2", b"entry")).unwrap();
    drop(reopened);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn a_failed_write_undoes_its_group() {
    let store =
      SqlCipherStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    store.begin_writes().unwrap();
    store.put(Table::Outbox, "1", b"entry").unwrap();
    // A write SQLite refuses: the table is gone.
    store
      .lock()
      .connection
      .execute_batch("ALTER TABLE kv RENAME TO kv_gone")
      .unwrap();
    assert!(store.put(Table::SyncState, "sync", b"cursor").is_err());
    store
      .lock()
      .connection
      .execute_batch("ALTER TABLE kv_gone RENAME TO kv")
      .unwrap();
    assert!(matches!(store.end_writes(), Err(StoreError::GroupFailed(_))));
    assert_eq!(store.get(Table::Outbox, "1").unwrap(), None);
  }

  #[test]
  fn writes_after_sqlite_rolled_the_group_back_are_refused() {
    let store =
      SqlCipherStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    store.begin_writes().unwrap();
    store.put(Table::Outbox, "1", b"entry").unwrap();
    // What SQLite does on a full disk: the transaction is gone.
    store.lock().connection.execute_batch("ROLLBACK").unwrap();
    assert!(matches!(
      store.put(Table::SyncState, "sync", b"cursor"),
      Err(StoreError::GroupFailed(_))
    ));
    assert!(store.end_writes().is_err());
    assert_eq!(
      store.get(Table::SyncState, "sync").unwrap(),
      None,
      "not committed on its own"
    );
    // The next group works again.
    crate::store::write_group(&store, || store.put(Table::Outbox, "2", b"entry")).unwrap();
    assert!(store.get(Table::Outbox, "2").unwrap().is_some());
  }

  #[test]
  fn a_database_of_the_first_layout_is_upgraded() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    {
      // Early 1.0 previews: no `in_channel` column, no version.
      let connection = Connection::open(&path).unwrap();
      keyed(&connection, &key);
      connection.execute_batch(SCHEMA_V1).unwrap();
      connection
        .execute(
          "INSERT INTO kv (tbl, key, value) VALUES ('saved_searches', 'legacy', x'6f6e65')",
          [],
        )
        .unwrap();
    }
    let store = SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
    assert_eq!(
      store.get(Table::SavedSearches, "legacy").unwrap(),
      Some(b"one".to_vec())
    );
    exercise_store(&store);
    let version: i64 = store
      .lock()
      .connection
      .query_row("PRAGMA user_version", [], |row| row.get(0))
      .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    drop(store);
    // Opening again finds nothing to do.
    SqlCipherStore::open(&path, &StaticKeySource::new(key)).unwrap();
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn a_database_of_a_newer_version_is_refused_untouched() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    drop(SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap());
    {
      let connection = Connection::open(&path).unwrap();
      keyed(&connection, &key);
      connection.execute_batch("PRAGMA user_version = 99").unwrap();
    }
    let opened = SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate()));
    let Err(error) = opened else {
      panic!("a newer layout opened");
    };
    assert!(
      matches!(
        error,
        StoreError::NewerSchema {
          found: 99,
          supported: SCHEMA_VERSION
        }
      ),
      "{error:?}"
    );
    assert!(error.is_unreadable());
    let connection = Connection::open(&path).unwrap();
    keyed(&connection, &key);
    let version: i64 = connection
      .query_row("PRAGMA user_version", [], |row| row.get(0))
      .unwrap();
    assert_eq!(version, 99, "left as it was");
    drop(connection);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn a_damaged_file_is_reported_as_unreadable() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    {
      let store = SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      for index in 0..200 {
        store.put(Table::Users, &index.to_string(), &[7u8; 512]).unwrap();
      }
    }
    // Overwrite pages past the first: SQLCipher's page MAC catches the damage on read.
    let mut bytes = std::fs::read(&path).unwrap();
    let length = bytes.len();
    for byte in &mut bytes[4096..length] {
      *byte ^= 0xa5;
    }
    std::fs::write(&path, &bytes).unwrap();
    let error = match SqlCipherStore::open(&path, &StaticKeySource::new(key)) {
      Err(error) => error,
      Ok(store) => store.list(Table::Users).unwrap_err(),
    };
    assert!(error.is_unreadable(), "{error:?}");
    std::fs::remove_dir_all(&directory).unwrap();
  }

  fn keyed(connection: &Connection, key: &SymmetricKey) {
    let hex: String = key
      .expose_secret()
      .iter()
      .map(|byte| format!("{byte:02x}"))
      .collect();
    connection
      .execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
      .unwrap();
  }

  fn scratch_dir() -> std::path::PathBuf {
    let directory = std::env::temp_dir().join(format!(
      "teamcraft-sqlcipher-{}",
      rand_core::RngCore::next_u64(&mut OsRng)
    ));
    std::fs::create_dir_all(&directory).unwrap();
    directory
  }
}
