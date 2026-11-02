//! Native [`LocalStore`] on SQLCipher (e2ee.md §9). The whole file is encrypted with a raw 256-bit
//! key from a [`DatabaseKeySource`]; nothing readable touches the disk.
//!
//! The layout has a version (`PRAGMA user_version`, [`MIGRATIONS`]); opening upgrades an older
//! database step by step and refuses a newer one instead of misreading it (issue #160).

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use teamcraft_proto::{ClientMessageId, ConversationId, Id, MessageId, ThreadId};
use zeroize::Zeroizing;

use super::sql_group::{
  Migration, MigrationKind, WriteGroup, run_migrations, sqlite_error as database_error,
};
use super::sqlite_log::{LOG_PRAGMAS, PurgeState};
use super::sqlite_wipe::wipe_free_pages;
use super::{
  DatabaseKeySource, LocalStore, MediaRange, MessageEdge, MessageFilter, MessageOrder, MessageSlot,
  StoreError, StoredMessage, Table, WriteTally,
};

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
  // The MLS state is stored per entry (issue #424); versions before would read none of it.
  Migration {
    version: 3,
    kind: MigrationKind::Marker,
  },
  // The feed's columns and indexes (issue #155), filled from the records.
  Migration {
    version: 4,
    kind: MigrationKind::Layout(migrate_to_v4),
  },
];
/// How a message record spells the media type of a picture or a video (`AttachmentRef`, compact JSON):
/// the bytes [`SqlCipherStore::media_messages`] looks for.
const MEDIA_IMAGE_MARKER: &[u8] = br#""media_type":"image/"#;
const MEDIA_VIDEO_MARKER: &[u8] = br#""media_type":"video/"#;
/// Rows a media query reads at a time at least.
const MEDIA_SCAN_BATCH: usize = 64;
/// IDs a query of the feed's reactions or replies names at most at once.
const IDS_PER_QUERY: usize = 256;
/// What a message is for the feed (issue #155): a row of it, an event the import's additions are
/// decided from, a reaction to another message, a shown reply. Written from the record by
/// [`FeedColumns`]; the feed reads trust them (they count positions), so a change of what they mean
/// fills them again in a layout step. Each has a partial index, so a page of the feed, the
/// reactions to its rows and their replies read only those.
const FEED_COLUMNS_V4: &str = "
  ALTER TABLE messages ADD COLUMN is_feed_row INTEGER NOT NULL DEFAULT 0;
  ALTER TABLE messages ADD COLUMN is_event INTEGER NOT NULL DEFAULT 0;
  ALTER TABLE messages ADD COLUMN reaction_target BLOB;
  ALTER TABLE messages ADD COLUMN is_reply INTEGER NOT NULL DEFAULT 0;
  CREATE INDEX messages_feed ON messages (conversation_id, sort_group, sort_value, client_message_id)
    WHERE is_feed_row = 1;
  CREATE INDEX messages_events ON messages (conversation_id, sort_group, sort_value, client_message_id)
    WHERE is_event = 1;
  CREATE INDEX messages_reactions ON messages (conversation_id, reaction_target)
    WHERE reaction_target IS NOT NULL;
  CREATE INDEX messages_replies ON messages (conversation_id, thread_id) WHERE is_reply = 1;
";
/// The feed's display order, ties broken as [`super::MessageOrder`] does.
const FEED_ORDER: &str = "ORDER BY sort_group, sort_value, client_message_id";

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

/// The feed's columns of a message ([`FEED_COLUMNS_V4`]).
struct FeedColumns {
  is_feed_row: bool,
  is_event: bool,
  maybe_reaction_target: Option<[u8; 16]>,
  is_reply: bool,
}

/// The connection and the group of writes open on it: one lock, so a write and the group it joins
/// never disagree.
struct Inner {
  connection: Connection,
  group: WriteGroup,
  purge: PurgeState,
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
    // doesn't stay readable to whoever later gets the key (issue #391). The write-ahead log holds
    // old pages (encrypted with the database key) until it is purged.
    connection
      .execute_batch(&format!("{LOG_PRAGMAS} PRAGMA secure_delete = ON;"))
      .map_err(database_error)?;
    run_migrations(&connection, MIGRATIONS)?;
    let mut group = WriteGroup::default();
    group.watch_other_connections(&connection)?;
    Ok(Self {
      inner: Mutex::new(Inner {
        connection,
        group,
        purge: PurgeState::new(),
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
    let Inner {
      connection, group, ..
    } = &mut *inner;
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

impl SqlCipherStore {
  /// [`LocalStore::feed_ranks`] of several orders: the feed's index walked once from its start to
  /// the last of them, reading keys only.
  fn feed_ranks_by_walk(
    &self,
    conversation: ConversationId,
    orders: &[MessageOrder],
  ) -> Result<Vec<usize>, StoreError> {
    let mut sorted: Vec<(usize, MessageOrder)> = orders.iter().copied().enumerate().collect();
    sorted.sort_by_key(|(_, order)| *order);
    let Some((_, last)) = sorted.last().copied() else {
      return Ok(Vec::new());
    };
    let inner = self.lock();
    let mut statement = inner
      .connection
      .prepare_cached(&format!(
        "SELECT sort_group, sort_value, client_message_id FROM messages
         WHERE conversation_id = ?1 AND is_feed_row = 1
           AND (sort_group, sort_value, client_message_id) < (?2, ?3, ?4) {FEED_ORDER}"
      ))
      .map_err(database_error)?;
    let mut rows = statement
      .query(params![
        conversation.snowflake().get(),
        last.group,
        last.value,
        last.client_message_id.as_slice()
      ])
      .map_err(database_error)?;
    let mut ranks = vec![0; orders.len()];
    let mut pending = sorted.into_iter().peekable();
    let mut before = 0usize;
    while let Some(row) = rows.next().map_err(database_error)? {
      let client: Vec<u8> = row.get(2).map_err(database_error)?;
      let row_order = MessageOrder {
        group: row.get(0).map_err(database_error)?,
        value: row.get(1).map_err(database_error)?,
        client_message_id: client.try_into().unwrap_or([0; 16]),
      };
      while let Some((at, _)) = pending.next_if(|(_, order)| *order <= row_order) {
        ranks[at] = before;
      }
      before += 1;
    }
    for (at, _) in pending {
      ranks[at] = before;
    }
    Ok(ranks)
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

  /// A range of the primary key `(tbl, key)`: keys compare bytewise (UTF-8 in code point order),
  /// so the keys starting with `prefix` are those from it up to the next string after all of them.
  fn list_prefix(&self, table: Table, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
    let Some(end) = prefix_end(prefix) else {
      return Ok(
        self
          .list(table)?
          .into_iter()
          .filter(|(key, _)| key.starts_with(prefix))
          .collect(),
      );
    };
    let inner = self.lock();
    let mut statement = inner
      .connection
      .prepare_cached("SELECT key, value FROM kv WHERE tbl = ?1 AND key >= ?2 AND key < ?3 ORDER BY key")
      .map_err(database_error)?;
    let rows = statement
      .query_map(params![table.name(), prefix, end], |row| {
        Ok((row.get(0)?, row.get(1)?))
      })
      .map_err(database_error)?;
    rows.collect::<Result<_, _>>().map_err(database_error)
  }

  fn upsert_message(&self, message: &StoredMessage) -> Result<(), StoreError> {
    let record = serde_json::to_vec(message).map_err(|error| StoreError::Corrupt(error.to_string()))?;
    let (sort_group, sort_value) = message.sort_key();
    let feed = FeedColumns::of(message);
    self.write(|connection| {
      connection.execute(
        "INSERT INTO messages
           (conversation_id, client_message_id, sort_group, sort_value, thread_id, is_hidden, in_channel, record,
            is_feed_row, is_event, reaction_target, is_reply)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
         ON CONFLICT (conversation_id, client_message_id) DO UPDATE SET
           sort_group = excluded.sort_group, sort_value = excluded.sort_value,
           thread_id = excluded.thread_id, is_hidden = excluded.is_hidden,
           in_channel = excluded.in_channel, record = excluded.record,
           is_feed_row = excluded.is_feed_row, is_event = excluded.is_event,
           reaction_target = excluded.reaction_target, is_reply = excluded.is_reply",
        params![
          message.conversation_id.snowflake().get(),
          message.client_message_id.as_bytes().as_slice(),
          sort_group,
          sort_value,
          message.maybe_thread_id.map(|thread| thread.snowflake().get()),
          message.is_hidden(),
          message.is_also_in_channel(),
          record,
          feed.is_feed_row,
          feed.is_event,
          feed.maybe_reaction_target.as_ref().map(<[u8; 16]>::as_slice),
          feed.is_reply,
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

  /// Server messages sort by their ID (`sort_group` 0), so the order index finds one directly. A
  /// line derived from a commit sorts by the commit's ID in the same group; it is no server
  /// message, whatever ID the server gave the commit.
  fn message_by_server_id(
    &self,
    conversation: ConversationId,
    server_id: MessageId,
  ) -> Result<Option<StoredMessage>, StoreError> {
    let inner = self.lock();
    let mut statement = inner
      .connection
      .prepare(
        "SELECT record FROM messages WHERE conversation_id = ?1 AND sort_group = 0 AND sort_value = ?2",
      )
      .map_err(database_error)?;
    let records: Vec<Vec<u8>> = statement
      .query_map(
        params![conversation.snowflake().get(), server_id.snowflake().get()],
        |row| row.get(0),
      )
      .map_err(database_error)?
      .collect::<Result<_, _>>()
      .map_err(database_error)?;
    for record in records {
      let message = decode_message(&record)?;
      if message.maybe_server_id == Some(server_id) {
        return Ok(Some(message));
      }
    }
    Ok(None)
  }

  /// Walks the order index from the end: server messages sort by their ID in `sort_group` 0, so the
  /// first record there with a server ID is the answer; lines this device derived share the group
  /// and are skipped.
  fn edge_server_id(
    &self,
    conversation: ConversationId,
    edge: MessageEdge,
  ) -> Result<Option<MessageId>, StoreError> {
    let order = match edge {
      MessageEdge::Oldest => "ASC",
      MessageEdge::Newest => "DESC",
    };
    let inner = self.lock();
    let mut statement = inner
      .connection
      .prepare_cached(&format!(
        "SELECT record FROM messages WHERE conversation_id = ?1 AND sort_group = 0 ORDER BY sort_value {order}"
      ))
      .map_err(database_error)?;
    let mut rows = statement
      .query(params![conversation.snowflake().get()])
      .map_err(database_error)?;
    while let Some(row) = rows.next().map_err(database_error)? {
      let record: Vec<u8> = row.get(0).map_err(database_error)?;
      // Placeholders don't count (`LocalStore::edge_server_id`).
      let message = decode_message(&record)?;
      if let Some(server_id) = message
        .maybe_server_id
        .filter(|_| message.maybe_unreadable.is_none())
      {
        return Ok(Some(server_id));
      }
    }
    Ok(None)
  }

  /// One place of the order index.
  fn messages_at(
    &self,
    conversation: ConversationId,
    slot: MessageSlot,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let conversation = conversation.snowflake().get();
    let inner = self.lock();
    let records: Vec<Vec<u8>> = match slot {
      MessageSlot::At(position) => inner
        .connection
        .prepare_cached(
          "SELECT record FROM messages WHERE conversation_id = ?1 AND sort_group = 0 AND sort_value = ?2",
        )
        .and_then(|mut statement| {
          statement
            .query_map(params![conversation, position.snowflake().get()], |row| {
              row.get(0)
            })?
            .collect()
        }),
      MessageSlot::Pending => inner
        .connection
        .prepare_cached(
          "SELECT record FROM messages WHERE conversation_id = ?1 AND sort_group = 1 ORDER BY sort_value",
        )
        .and_then(|mut statement| {
          statement
            .query_map(params![conversation], |row| row.get(0))?
            .collect()
        }),
    }
    .map_err(database_error)?;
    records.iter().map(|record| decode_message(record)).collect()
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
    // The columns only preselect: a row written by an older version may say "listed" for a kind
    // this one hides (an admin's join as a message, issue #811), so the record decides.
    let mut messages = Vec::with_capacity(records.len());
    for record in &records {
      let message = decode_message(record)?;
      if message.matches(filter) {
        messages.push(message);
      }
    }
    Ok(messages)
  }

  /// Walks the order index from the range's start and tests the records' bytes for a picture or
  /// video type, without decoding them: only matches are decoded (and checked again, as a text can
  /// contain the same words). Stops at `limit`, so a page costs what it returns plus the messages
  /// without media in between.
  fn media_messages(
    &self,
    conversation: ConversationId,
    range: MediaRange,
    limit: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let inner = self.lock();
    let (comparison, order, mut maybe_cursor) = match range {
      MediaRange::Older(maybe_from) => ("<", "DESC", maybe_from.map(|from| from.snowflake().get())),
      MediaRange::Newer(from) => (">", "ASC", Some(from.snowflake().get())),
    };
    let batch = i64::try_from(limit.saturating_mul(2).max(MEDIA_SCAN_BATCH)).unwrap_or(i64::MAX);
    let mut found = Vec::new();
    while found.len() < limit {
      let cursor_condition = match maybe_cursor {
        Some(_) => format!("AND sort_value {comparison} ?4"),
        None => String::new(),
      };
      let sql = format!(
        "SELECT sort_value, record FROM messages
         WHERE conversation_id = ?1 AND sort_group = 0 AND is_hidden = 0
           AND (instr(record, ?2) > 0 OR instr(record, ?3) > 0) {cursor_condition}
         ORDER BY sort_value {order} LIMIT {batch}"
      );
      let mut statement = inner.connection.prepare(&sql).map_err(database_error)?;
      let mut values = vec![
        Value::Integer(conversation.snowflake().get()),
        Value::Blob(MEDIA_IMAGE_MARKER.to_vec()),
        Value::Blob(MEDIA_VIDEO_MARKER.to_vec()),
      ];
      values.extend(maybe_cursor.map(Value::Integer));
      let rows: Vec<(i64, Vec<u8>)> = statement
        .query_map(params_from_iter(values), |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(database_error)?
        .collect::<Result<_, _>>()
        .map_err(database_error)?;
      let is_last_batch = i64::try_from(rows.len()).is_ok_and(|read| read < batch);
      for (sort_value, record) in rows {
        maybe_cursor = Some(sort_value);
        let message = decode_message(&record)?;
        if message.has_media() {
          found.push(message);
          if found.len() == limit {
            break;
          }
        }
      }
      if is_last_batch {
        break;
      }
    }
    Ok(found)
  }

  fn indexes_feed(&self) -> bool {
    true
  }

  fn feed_len(&self, conversation: ConversationId) -> Result<usize, StoreError> {
    let count: i64 = self
      .lock()
      .connection
      .prepare_cached("SELECT count(*) FROM messages WHERE conversation_id = ?1 AND is_feed_row = 1")
      .and_then(|mut statement| {
        statement.query_row(params![conversation.snowflake().get()], |row| row.get(0))
      })
      .map_err(database_error)?;
    Ok(usize::try_from(count).unwrap_or(0))
  }

  /// A range of the feed's index; the offset steps through the index only, not the records.
  fn feed_rows(
    &self,
    conversation: ConversationId,
    start: usize,
    len: usize,
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let inner = self.lock();
    let records: Vec<Vec<u8>> = inner
      .connection
      .prepare_cached(&format!(
        "SELECT record FROM messages WHERE conversation_id = ?1 AND is_feed_row = 1 {FEED_ORDER}
         LIMIT ?2 OFFSET ?3"
      ))
      .and_then(|mut statement| {
        statement
          .query_map(
            params![
              conversation.snowflake().get(),
              i64::try_from(len).unwrap_or(i64::MAX),
              i64::try_from(start).unwrap_or(i64::MAX)
            ],
            |row| row.get(0),
          )?
          .collect()
      })
      .map_err(database_error)?;
    records.iter().map(|record| decode_message(record)).collect()
  }

  /// One order is counted from the end of the index, where the lists' anchors are; several in one
  /// walk from its start up to the last of them.
  fn feed_ranks(
    &self,
    conversation: ConversationId,
    orders: &[MessageOrder],
  ) -> Result<Vec<usize>, StoreError> {
    match orders {
      [] => Ok(Vec::new()),
      [order] => {
        let len = self.feed_len(conversation)?;
        let after: i64 = self
          .lock()
          .connection
          .prepare_cached(
            "SELECT count(*) FROM messages WHERE conversation_id = ?1 AND is_feed_row = 1
               AND (sort_group, sort_value, client_message_id) >= (?2, ?3, ?4)",
          )
          .and_then(|mut statement| {
            statement.query_row(
              params![
                conversation.snowflake().get(),
                order.group,
                order.value,
                order.client_message_id.as_slice()
              ],
              |row| row.get(0),
            )
          })
          .map_err(database_error)?;
        Ok(vec![len.saturating_sub(usize::try_from(after).unwrap_or(0))])
      },
      orders => self.feed_ranks_by_walk(conversation, orders),
    }
  }

  fn event_messages(&self, conversation: ConversationId) -> Result<Vec<StoredMessage>, StoreError> {
    let inner = self.lock();
    let records: Vec<Vec<u8>> = inner
      .connection
      .prepare_cached(&format!(
        "SELECT record FROM messages WHERE conversation_id = ?1 AND is_event = 1 {FEED_ORDER}"
      ))
      .and_then(|mut statement| {
        statement
          .query_map(params![conversation.snowflake().get()], |row| row.get(0))?
          .collect()
      })
      .map_err(database_error)?;
    records.iter().map(|record| decode_message(record)).collect()
  }

  fn reactions_to(
    &self,
    conversation: ConversationId,
    targets: &[ClientMessageId],
  ) -> Result<Vec<StoredMessage>, StoreError> {
    let inner = self.lock();
    let mut reactions = Vec::new();
    for chunk in targets.chunks(IDS_PER_QUERY) {
      let sql = format!(
        "SELECT record FROM messages WHERE conversation_id = ?1 AND reaction_target IN ({})",
        placeholders(chunk.len())
      );
      let mut values = vec![Value::Integer(conversation.snowflake().get())];
      values.extend(chunk.iter().map(|target| Value::Blob(target.as_bytes().to_vec())));
      let records: Vec<Vec<u8>> = inner
        .connection
        .prepare(&sql)
        .and_then(|mut statement| {
          statement
            .query_map(params_from_iter(values), |row| row.get(0))?
            .collect()
        })
        .map_err(database_error)?;
      for record in &records {
        reactions.push(decode_message(record)?);
      }
    }
    reactions.sort_by_key(StoredMessage::order);
    Ok(reactions)
  }

  fn reply_counts(
    &self,
    conversation: ConversationId,
    threads: &[ThreadId],
  ) -> Result<Vec<usize>, StoreError> {
    let inner = self.lock();
    let mut counts = vec![0; threads.len()];
    for (chunk_at, chunk) in threads.chunks(IDS_PER_QUERY).enumerate() {
      let sql = format!(
        "SELECT thread_id, count(*) FROM messages
         WHERE conversation_id = ?1 AND is_reply = 1 AND thread_id IN ({}) GROUP BY thread_id",
        placeholders(chunk.len())
      );
      let mut values = vec![conversation.snowflake().get()];
      values.extend(chunk.iter().map(|thread| thread.snowflake().get()));
      let found: Vec<(i64, i64)> = inner
        .connection
        .prepare(&sql)
        .and_then(|mut statement| {
          statement
            .query_map(params_from_iter(values), |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect()
        })
        .map_err(database_error)?;
      for (thread, count) in found {
        for (at, asked) in chunk.iter().enumerate() {
          if asked.snowflake().get() == thread {
            counts[chunk_at * IDS_PER_QUERY + at] = usize::try_from(count).unwrap_or(0);
          }
        }
      }
    }
    Ok(counts)
  }

  fn delete_messages(&self, conversation: ConversationId) -> Result<(), StoreError> {
    self.write(|connection| {
      connection.execute(
        "DELETE FROM messages WHERE conversation_id = ?1",
        params![conversation.snowflake().get()],
      )
    })
  }

  fn delete_message(
    &self,
    conversation: ConversationId,
    client_message_id: ClientMessageId,
  ) -> Result<(), StoreError> {
    self.write(|connection| {
      connection.execute(
        "DELETE FROM messages WHERE conversation_id = ?1 AND client_message_id = ?2",
        params![
          conversation.snowflake().get(),
          client_message_id.as_bytes().as_slice()
        ],
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
      purge.run_if_due(connection);
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
    purge.force();
    if !group.is_open() {
      purge.run_if_due(connection);
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
      purge.run_if_due(connection);
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

impl Drop for SqlCipherStore {
  fn drop(&mut self) {
    // Closing the app is the last chance to clear what the interval kept back.
    let mut inner = self.lock();
    let Inner {
      connection, purge, ..
    } = &mut *inner;
    if purge.is_due {
      purge.force();
      purge.run_if_due(connection);
    }
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

/// Version 4: the feed's columns and indexes, filled from every record (issue #155). A record that
/// doesn't decode keeps the defaults, as no row of the feed; reading it fails as before.
fn migrate_to_v4(connection: &Connection) -> Result<(), StoreError> {
  connection
    .execute_batch(FEED_COLUMNS_V4)
    .map_err(database_error)?;
  let mut select = connection
    .prepare("SELECT rowid, record FROM messages")
    .map_err(database_error)?;
  let mut update = connection
    .prepare(
      "UPDATE messages SET is_feed_row = ?2, is_event = ?3, reaction_target = ?4, is_reply = ?5
       WHERE rowid = ?1",
    )
    .map_err(database_error)?;
  let mut rows = select.query([]).map_err(database_error)?;
  let mut undecodable = 0usize;
  while let Some(row) = rows.next().map_err(database_error)? {
    let rowid: i64 = row.get(0).map_err(database_error)?;
    let record: Vec<u8> = row.get(1).map_err(database_error)?;
    let Ok(message) = decode_message(&record) else {
      undecodable += 1;
      continue;
    };
    let feed = FeedColumns::of(&message);
    update
      .execute(params![
        rowid,
        feed.is_feed_row,
        feed.is_event,
        feed.maybe_reaction_target.as_ref().map(<[u8; 16]>::as_slice),
        feed.is_reply,
      ])
      .map_err(database_error)?;
  }
  if undecodable > 0 {
    tracing::warn!(
      undecodable,
      "messages left out of the feed's index: their records don't decode"
    );
  }
  Ok(())
}

impl FeedColumns {
  fn of(message: &StoredMessage) -> Self {
    Self {
      is_feed_row: message.is_feed_row(),
      is_event: message.is_event(),
      maybe_reaction_target: message.reaction_target().map(|target| *target.as_bytes()),
      is_reply: message.counts_as_reply(),
    }
  }
}

/// `?2, ?3, …` for `count` values after the conversation.
fn placeholders(count: usize) -> String {
  (2..count + 2)
    .map(|at| format!("?{at}"))
    .collect::<Vec<_>>()
    .join(", ")
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

/// The first string after every string that starts with `prefix`: its last character raised by
/// one. `None` when there is none (empty, or only the highest code points).
fn prefix_end(prefix: &str) -> Option<String> {
  let mut chars: Vec<char> = prefix.chars().collect();
  while let Some(last) = chars.pop() {
    let next = (u32::from(last) + 1..=u32::from(char::MAX)).find_map(char::from_u32);
    if let Some(next) = next {
      chars.push(next);
      return Some(chars.into_iter().collect());
    }
  }
  None
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
  fn a_prefix_ends_after_every_key_that_starts_with_it() {
    assert_eq!(prefix_end("7/").as_deref(), Some("70"));
    assert_eq!(prefix_end("a\u{10FFFF}").as_deref(), Some("b"));
    assert_eq!(prefix_end(""), None);
    assert_eq!(prefix_end("\u{10FFFF}"), None);
  }

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

  /// Issues #811, #818: an admin's join or an archiving as a message that an older version stored
  /// as listed is hidden now, without rewriting the database.
  #[test]
  fn a_row_listed_by_an_older_version_follows_what_this_one_hides() {
    use teamcraft_proto::{Id, Snowflake};

    use crate::store::{ConversationEvent, MessageContent};

    let store =
      SqlCipherStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    let id = |raw: i64| Snowflake::new(raw).unwrap();
    let conversation = ConversationId::from_snowflake(id(1));
    let announced = StoredMessage {
      conversation_id: conversation,
      client_message_id: ClientMessageId::from_bytes([1; 16]),
      maybe_server_id: Some(MessageId::from_snowflake(id(100))),
      sender_user_id: teamcraft_proto::UserId::from_snowflake(id(2)),
      maybe_thread_id: None,
      sent_at_ms: 1_000,
      content: MessageContent::Event {
        event: ConversationEvent::AdminJoined {
          user: teamcraft_proto::UserId::from_snowflake(id(3)),
          is_approved: false,
        },
      },
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    };
    let archived = StoredMessage {
      client_message_id: ClientMessageId::from_bytes([2; 16]),
      maybe_server_id: Some(MessageId::from_snowflake(id(101))),
      content: MessageContent::Event {
        event: ConversationEvent::Archived,
      },
      ..announced.clone()
    };
    store.upsert_message(&announced).unwrap();
    store.upsert_message(&archived).unwrap();
    store
      .lock()
      .connection
      .execute("UPDATE messages SET is_hidden = 0", [])
      .unwrap();

    assert!(
      store
        .messages(conversation, MessageFilter::TopLevel)
        .unwrap()
        .is_empty()
    );
    assert_eq!(
      store.messages(conversation, MessageFilter::All).unwrap(),
      vec![announced, archived]
    );
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
    let lost = Some(WriteTally { lost: 1, foreign: 0 });
    assert_eq!(store.write_tally(), lost);
    crate::store::write_group(&store, || store.put(Table::Outbox, "2", b"entry")).unwrap();
    assert_eq!(store.write_tally(), lost, "a commit changes nothing");
  }

  /// Issue #424 review, H1: the layout marks the MLS state per entry; 1.0.8 (its steps end before
  /// the marker) refuses such a database instead of reading no MLS state.
  #[test]
  fn a_version_before_the_mls_state_per_entry_refuses_the_database() {
    let store =
      SqlCipherStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    let as_in_1_0_8 = &MIGRATIONS[..2];
    assert!(matches!(
      run_migrations(&store.lock().connection, as_in_1_0_8),
      Err(StoreError::NewerSchema { found, supported: 2 }) if found == SCHEMA_VERSION
    ));
  }

  /// Issue #155: a database from before the feed's columns gets them, filled from its records.
  #[test]
  fn a_database_before_the_feed_columns_has_them_filled() {
    use crate::store::tests::{id, message};
    use crate::store::{MessageContent, MessageOrder};

    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let conversation = id(5);
    let root = message(conversation, 1, Some(100), None, 1);
    let reply = message(conversation, 2, Some(110), Some(100), 2);
    let mut reaction = message(conversation, 3, Some(120), None, 3);
    reaction.content = MessageContent::Reaction {
      target: root.client_message_id,
      emoji: "+1".into(),
      is_added: true,
    };
    let later = message(conversation, 4, Some(130), None, 4);
    {
      let connection = Connection::open(&path).unwrap();
      keyed(&connection, &key);
      run_migrations(&connection, &MIGRATIONS[..3]).unwrap();
      for stored in [&root, &reply, &reaction, &later] {
        let (sort_group, sort_value) = stored.sort_key();
        connection
          .execute(
            "INSERT INTO messages
               (conversation_id, client_message_id, sort_group, sort_value, thread_id, is_hidden, in_channel, record)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)",
            params![
              conversation.snowflake().get(),
              stored.client_message_id.as_bytes().as_slice(),
              sort_group,
              sort_value,
              stored.maybe_thread_id.map(|thread| thread.snowflake().get()),
              stored.is_hidden(),
              serde_json::to_vec(stored).unwrap(),
            ],
          )
          .unwrap();
      }
    }
    let store = SqlCipherStore::open(&path, &StaticKeySource::new(key)).unwrap();
    assert_eq!(store.feed_len(conversation).unwrap(), 2);
    assert_eq!(store.feed_rows(conversation, 1, 5).unwrap(), vec![later.clone()]);
    assert_eq!(store.feed_ranks(conversation, &[later.order()]).unwrap(), [1]);
    let past_all = MessageOrder {
      value: i64::MAX,
      ..later.order()
    };
    assert_eq!(
      store
        .feed_ranks(conversation, &[past_all, root.order(), later.order()])
        .unwrap(),
      [2, 0, 1]
    );
    assert_eq!(
      store
        .reactions_to(conversation, &[root.client_message_id])
        .unwrap(),
      vec![reaction]
    );
    assert_eq!(store.reply_counts(conversation, &[id(100)]).unwrap(), [1]);
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #424 review, H2: a commit of another connection to the same file (another process)
  /// shows in the tally, so the next MLS save writes the whole state.
  #[test]
  fn another_connections_commit_shows_in_the_tally() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let ours = SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
    let theirs = SqlCipherStore::open(&path, &StaticKeySource::new(key)).unwrap();
    ours.put(Table::Users, "1", b"ours").unwrap();
    assert_eq!(ours.write_tally(), Some(WriteTally::default()));
    theirs.put(Table::Users, "2", b"theirs").unwrap();
    assert_eq!(ours.write_tally().map(|tally| tally.foreign), Some(1));
    drop((ours, theirs));
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #424: a save of the MLS state in a group SQLite rolled back (a full disk) went
  /// through as far as the save could tell. The tally tells the next save, which writes the
  /// whole state again: the database holds what the provider holds, also after a restart.
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
      let store = SqlCipherStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      let saved = SavedMlsStorage::default();
      set(b"epoch", Some(b"1"));
      set(b"spent key package", Some(b"secret"));
      save_mls_storage(&store, &provider, &saved).unwrap();

      set(b"epoch", Some(b"2"));
      set(b"spent key package", None);
      store.begin_writes().unwrap();
      save_mls_storage(&store, &provider, &saved).unwrap();
      store.lock().connection.execute_batch("ROLLBACK").unwrap();
      assert!(store.end_writes().is_err());

      set(b"next group", Some(b"3"));
      save_mls_storage(&store, &provider, &saved).unwrap();
    }
    let reopened = SqlCipherStore::open(&path, &StaticKeySource::new(key)).unwrap();
    let (loaded, _) = load_mls_storage(&reopened).unwrap();
    assert_eq!(
      *loaded.storage().values.read().unwrap(),
      *provider.storage().values.read().unwrap()
    );
    drop(reopened);
    std::fs::remove_dir_all(&directory).unwrap();
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
