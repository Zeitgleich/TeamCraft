//! Native [`LocalStore`] on SQLCipher (e2ee.md §9). The whole file is encrypted with a raw 256-bit
//! key from a [`DatabaseKeySource`]; nothing readable touches the disk.

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use teamcraft_proto::{ClientMessageId, ConversationId, Id};
use zeroize::Zeroizing;

use super::{DatabaseKeySource, LocalStore, MessageFilter, StoreError, StoredMessage, Table};

const SCHEMA: &str = "
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
  connection: Mutex<Connection>,
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
    connection.execute_batch(SCHEMA).map_err(database_error)?;
    migrate(&connection)?;
    Ok(Self {
      connection: Mutex::new(connection),
    })
  }

  fn lock(&self) -> MutexGuard<'_, Connection> {
    // A poisoned lock only means another thread panicked; SQLite keeps the connection consistent.
    self
      .connection
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

impl LocalStore for SqlCipherStore {
  fn get(&self, table: Table, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    self
      .lock()
      .query_row(
        "SELECT value FROM kv WHERE tbl = ?1 AND key = ?2",
        params![table.name(), key],
        |row| row.get(0),
      )
      .optional()
      .map_err(database_error)
  }

  fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), StoreError> {
    self
      .lock()
      .execute(
        "INSERT INTO kv (tbl, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (tbl, key) DO UPDATE SET value = excluded.value",
        params![table.name(), key, value],
      )
      .map(|_| ())
      .map_err(database_error)
  }

  fn delete(&self, table: Table, key: &str) -> Result<(), StoreError> {
    self
      .lock()
      .execute(
        "DELETE FROM kv WHERE tbl = ?1 AND key = ?2",
        params![table.name(), key],
      )
      .map(|_| ())
      .map_err(database_error)
  }

  fn list(&self, table: Table) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let connection = self.lock();
    let mut statement = connection
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
    self
      .lock()
      .execute(
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
      .map(|_| ())
      .map_err(database_error)
  }

  fn message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    let maybe_record: Option<Vec<u8>> = self
      .lock()
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

  fn messages(
    &self,
    conversation: ConversationId,
    filter: MessageFilter,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let connection = self.lock();
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
    let mut statement = connection.prepare(&sql).map_err(database_error)?;
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
    self
      .lock()
      .execute(
        "DELETE FROM messages WHERE conversation_id = ?1",
        params![conversation.snowflake().get()],
      )
      .map(|_| ())
      .map_err(database_error)
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

/// Schema changes after the first version, applied in order to older databases.
fn migrate(connection: &Connection) -> Result<(), StoreError> {
  // Thread replies also sent to the conversation (`MessageMeta::is_also_in_channel`).
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

fn database_error(error: rusqlite::Error) -> StoreError {
  StoreError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::secret::SymmetricKey;

  use super::*;
  use crate::store::StaticKeySource;
  use crate::store::tests::exercise_store;

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
    std::fs::remove_dir_all(&directory).unwrap();
  }
}
