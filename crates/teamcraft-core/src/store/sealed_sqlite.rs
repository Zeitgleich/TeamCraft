//! [`LocalStore`] on plain SQLite with sealed records (e2ee.md §9.1), for platforms where SQLCipher
//! can't link a crypto library: Android (the NDK ships no libcrypto) and anywhere else that
//! enables the `sealed-sqlite` feature. The file holds only lookup IDs and ciphertext.
//!
//! The layout has a version (`PRAGMA user_version`, as in the SQLCipher store); a newer one is
//! refused (issue #160).

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rand_core::{OsRng, RngCore};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use teamcraft_crypto::local_store::{LOOKUP_ID_LEN, LookupId};

use super::log_purger::{LogPurge, LogPurgeLimits, LogPurger, Ticket};
use super::sealed::{RecordBackend, SealedStore, SealedStoreLimits};
use super::sql_group::{
  Migration, MigrationKind, WriteGroup, check_layout_left_to_app, run_migrations,
  sqlite_error as database_error,
};
use super::sqlite_log::LOG_PRAGMAS;
use super::sqlite_wipe::wipe_free_pages;
use super::{CommitJournal, CommitStamp, DatabaseKeySource, StoreError, WriteTally};

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
  // The catalog (issue #155): versions before would write messages and entries it doesn't list.
  // The store builds it when it opens (`SealedStore::open_backend`), resumably.
  Migration {
    version: 4,
    kind: MigrationKind::Marker,
  },
  // The catalog's feed and relations indexes (issue #155): versions before would write messages
  // they don't list. The store builds them when it opens, like the catalog itself.
  Migration {
    version: 5,
    kind: MigrationKind::Marker,
  },
];
/// How much of the database file reads map into memory instead of copying it.
const MMAP_BYTES: u64 = 256 * 1024 * 1024;
/// How long a write waits for another connection's lock (a checkpoint of the log runs into one).
const BUSY_TIMEOUT: Duration = Duration::from_secs(1);
/// The number of the last commit and the connection that made it ([`CommitStamp`], issue #1476).
/// Outside the numbered layout: versions before ignore it, and a store without it (the iOS
/// notification extension opened it first) numbers no commits until the app opens it.
const COMMIT_STAMP_TABLE: &str = "
  CREATE TABLE IF NOT EXISTS commit_stamp (
    id INTEGER PRIMARY KEY CHECK (id = 0),
    sequence INTEGER NOT NULL,
    writer INTEGER NOT NULL
  );
  INSERT OR IGNORE INTO commit_stamp (id, sequence, writer) VALUES (0, 0, 0);
";
const SCHEMA_V1: &str = "
  CREATE TABLE IF NOT EXISTS sealed_records (
    id BLOB PRIMARY KEY,
    value BLOB NOT NULL
  ) WITHOUT ROWID;
";

/// The encrypted store on SQLite.
pub type SealedSqliteStore = SealedStore<SqliteRecords>;

/// How a connection takes part in the purge of the write-ahead log.
enum Purging<'a> {
  /// A database in memory: no log.
  None,
  /// The writing connection: runs the purge for the database at the path.
  Runs(&'a Path),
  /// A connection that only reads: reports its transactions to the writer's purge.
  Reports(&'a Path),
}

/// The SQLite table holding the sealed records.
pub struct SqliteRecords {
  inner: Mutex<Inner>,
  /// The purge of the write-ahead log this connection reports its transactions to; `None` for a
  /// database in memory, which has no log.
  maybe_purge: Option<Arc<LogPurge>>,
  /// The writing connection's purge thread (issue #1684); `None` for a connection that only reads.
  maybe_purger: Option<LogPurger>,
}

/// The connection and its open group of writes, under one lock.
struct Inner {
  connection: Connection,
  group: WriteGroup,
  /// The transaction open on the connection, as [`SqliteRecords::maybe_purge`] counts it.
  maybe_ticket: Option<Ticket>,
  /// The last commit this connection knows of: seen at the last look at the tally, or its own.
  maybe_known_stamp: Option<CommitStamp>,
  /// The number the open group's commit takes ([`RecordBackend::stage_commit`]).
  maybe_staged_stamp: Option<CommitStamp>,
  /// A read transaction is open ([`RecordBackend::begin_read`]).
  is_reading: bool,
  /// This connection's mark on the commits it numbers ([`CommitStamp::writer`]).
  writer: u64,
}

impl SealedStore<SqliteRecords> {
  /// Opens (or creates) the database at `path`, with the default cache limits. Records are read
  /// when they are asked for.
  pub fn open(path: &Path, key_source: &impl DatabaseKeySource) -> Result<Self, StoreError> {
    Self::open_with_limits(path, key_source, SealedStoreLimits::default())
  }

  /// [`Self::open`] with the caches bounded by `limits` (the iOS notification extension keeps
  /// little: [`SealedStoreLimits::minimal`]).
  pub fn open_with_limits(
    path: &Path,
    key_source: &impl DatabaseKeySource,
    limits: SealedStoreLimits,
  ) -> Result<Self, StoreError> {
    Self::from_connection(
      Connection::open(path).map_err(database_error)?,
      key_source,
      limits,
      Purging::Runs(path),
    )
  }

  /// [`Self::open_with_limits`] for a process that must stay small and runs next to the app (the
  /// iOS notification extension): it never builds the catalog of a store that lacks one, however
  /// long the history, runs no step of the layout and no upkeep (`VACUUM` takes memory and time it
  /// doesn't have, and would keep the app out of the store), and creates no database.
  ///
  /// A store that needs such a step is [`StoreError::LayoutBehind`], found on a read-only connection
  /// so that even its closing changes nothing; the app migrates it when it opens.
  pub fn open_without_building(
    path: &Path,
    key_source: &impl DatabaseKeySource,
    limits: SealedStoreLimits,
  ) -> Result<Self, StoreError> {
    Self::check_layout_is_left_to_app(path)?;
    let connection = Connection::open_with_flags(
      path,
      OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(database_error)?;
    Self::set_up_connection(&connection)?;
    let backend = Self::records_on(connection, Purging::Runs(path))?;
    SealedStore::open_backend_without_building(backend, &key_source.database_key()?, limits)
  }

  fn check_layout_is_left_to_app(path: &Path) -> Result<(), StoreError> {
    let connection = Connection::open_with_flags(
      path,
      OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(database_error)?;
    connection.busy_timeout(BUSY_TIMEOUT).map_err(database_error)?;
    check_layout_left_to_app(&connection, MIGRATIONS)
  }

  /// A second connection to the store at `path`, which only reads: the app's read threads read
  /// beside the engine's groups of writes instead of waiting for them (issue #1476). It follows the
  /// commits the writing connection stages in `journal`
  /// ([`SealedStore::publish_commits`]), so its caches stay warm across them. Opened after the
  /// writing connection: it builds no catalog and runs no step of the layout.
  pub fn open_reader(
    path: &Path,
    key_source: &impl DatabaseKeySource,
    limits: SealedStoreLimits,
    journal: Arc<CommitJournal>,
  ) -> Result<Self, StoreError> {
    // URI: like `open`, so a test can open both connections on a VFS of its own
    // (`file:…?vfs=`, tests/suite/slow_disk); an app's path is never a `file:` URI.
    let connection = Connection::open_with_flags(
      path,
      OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(database_error)?;
    Self::set_up_connection(&connection)?;
    connection
      .execute_batch("PRAGMA query_only = ON;")
      .map_err(database_error)?;
    let backend = Self::records_on(connection, Purging::Reports(path))?;
    let store = SealedStore::open_backend_without_building(backend, &key_source.database_key()?, limits)?;
    store.follow_commits(journal);
    Ok(store)
  }

  /// A database that lives only in memory, for tests.
  pub fn open_in_memory(key_source: &impl DatabaseKeySource) -> Result<Self, StoreError> {
    Self::from_connection(
      Connection::open_in_memory().map_err(database_error)?,
      key_source,
      SealedStoreLimits::default(),
      Purging::None,
    )
  }

  fn from_connection(
    connection: Connection,
    key_source: &impl DatabaseKeySource,
    limits: SealedStoreLimits,
    purging: Purging<'_>,
  ) -> Result<Self, StoreError> {
    let backend = Self::backend_of(connection, purging)?;
    SealedStore::open_backend(backend, &key_source.database_key()?, limits)
  }

  fn backend_of(connection: Connection, purging: Purging<'_>) -> Result<SqliteRecords, StoreError> {
    Self::set_up_connection(&connection)?;
    run_migrations(&connection, MIGRATIONS)?;
    connection
      .execute_batch(COMMIT_STAMP_TABLE)
      .map_err(database_error)?;
    Self::records_on(connection, purging)
  }

  /// Changes when the write-ahead log is purged (docs/e2ee.md §4.3.1); on the writing connection.
  pub fn set_log_purge_limits(&self, limits: LogPurgeLimits) {
    if let Some(purge) = &self.backend().maybe_purge {
      purge.set_limits(limits);
    }
  }

  fn set_up_connection(connection: &Connection) -> Result<(), StoreError> {
    // secure_delete: freed pages are zeroed, so a deleted record's ciphertext doesn't linger in
    // them (issue #391); the write-ahead log is cleared by `purge_freed`. Records are read one by
    // one under pseudorandom IDs (issue #155): mapped, a read is no system call, and the pages
    // are the file system's cache, not the app's memory.
    connection
      .execute_batch(&format!(
        "{LOG_PRAGMAS} PRAGMA secure_delete = ON; PRAGMA mmap_size = {MMAP_BYTES};"
      ))
      .map_err(database_error)?;
    connection.busy_timeout(BUSY_TIMEOUT).map_err(database_error)
  }

  fn records_on(connection: Connection, purging: Purging<'_>) -> Result<SqliteRecords, StoreError> {
    // Before anything is read: a commit of another connection after it counts (the caches drop
    // what they hold).
    // Read before the data version: a commit in between then counts once more, never not at all.
    let maybe_known_stamp = stamp_of(&connection);
    let mut group = WriteGroup::default();
    group.watch_other_connections(&connection)?;
    let (maybe_purge, maybe_purger) = match purging {
      Purging::None => (None, None),
      Purging::Runs(path) => {
        LogPurger::take_over_checkpoints(&connection)?;
        let purger = LogPurger::start(path)?;
        (Some(Arc::clone(purger.purge())), Some(purger))
      },
      Purging::Reports(path) => {
        let maybe_purge = LogPurge::of_database(path);
        if maybe_purge.is_none() {
          // Its reads then keep the log without the purge knowing; the purge still retries.
          tracing::warn!("a reading connection opened without the writer's log purge");
        }
        (maybe_purge, None)
      },
    };
    Ok(SqliteRecords {
      inner: Mutex::new(Inner {
        connection,
        group,
        maybe_ticket: None,
        maybe_known_stamp,
        maybe_staged_stamp: None,
        is_reading: false,
        writer: OsRng.next_u64(),
      }),
      maybe_purge,
      maybe_purger,
    })
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

  fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
    self
      .lock()
      .connection
      .prepare_cached("SELECT value FROM sealed_records WHERE id = ?1")
      .and_then(|mut statement| {
        statement
          .query_row(params![id.0.as_slice()], |row| row.get(0))
          .optional()
      })
      .map_err(database_error)
  }

  fn records_after(
    &self,
    maybe_after: Option<&LookupId>,
    limit: usize,
  ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
    let after: &[u8] = maybe_after.map_or(&[], |id| id.0.as_slice());
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let inner = self.lock();
    let mut statement = inner
      .connection
      .prepare_cached("SELECT id, value FROM sealed_records WHERE id > ?1 ORDER BY id LIMIT ?2")
      .map_err(database_error)?;
    let rows = statement
      .query_map(params![after, limit], |row| {
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

  fn fail_group(&self) {
    self.lock().group.note_failure();
  }

  /// A read transaction: another process's commit between two reads of a walk through the catalog
  /// is not seen until it ends.
  fn begin_read(&self) -> Result<(), StoreError> {
    let mut inner = self.lock();
    if inner.group.is_open() || inner.is_reading {
      return Ok(());
    }
    inner.connection.execute_batch("BEGIN").map_err(database_error)?;
    inner.is_reading = true;
    self.note_transaction(&mut inner);
    Ok(())
  }

  fn end_read(&self) {
    let mut inner = self.lock();
    if std::mem::take(&mut inner.is_reading) {
      // Deliberately ignored: nothing was written, and the next statement reports a broken connection.
      let _ = inner.connection.execute_batch("COMMIT");
    }
    self.note_transaction(&mut inner);
  }

  fn before_writes(&self) {
    if let Some(purge) = &self.maybe_purge {
      purge.wait_before_writes();
    }
  }

  fn begin_writes(&self) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let Inner {
      connection, group, ..
    } = &mut *inner;
    let began = group.begin(connection);
    self.note_transaction(&mut inner);
    began
  }

  fn end_writes(&self) -> Result<(), StoreError> {
    let mut inner = self.lock();
    let Inner {
      connection, group, ..
    } = &mut *inner;
    let ended = group.end(connection);
    if ended.is_ok()
      && !inner.group.is_open()
      && let Some(purger) = &self.maybe_purger
    {
      purger.after_commit();
    }
    let maybe_staged = inner.maybe_staged_stamp.take();
    if ended.is_ok() && maybe_staged.is_some() {
      inner.maybe_known_stamp = maybe_staged;
    }
    self.note_transaction(&mut inner);
    ended
  }

  /// Has the log purged (docs/e2ee.md §4.3.1): its thread writes the log back into the database
  /// (whose freed pages `secure_delete` zeroes) and truncates it, so no old page version stays in
  /// the log file, as soon as no transaction of this process keeps the log, and within the
  /// purge's maximum delay. Inside a group of writes it waits for the group's commit: a
  /// checkpoint before would write half of the group to the database.
  fn purge_freed(&self) -> Result<(), StoreError> {
    if let Some(purge) = &self.maybe_purge {
      purge.request_now();
    }
    Ok(())
  }

  fn purge_freed_soon(&self) {
    if let Some(purge) = &self.maybe_purge {
      purge.request_soon();
    }
  }

  fn wait_for_purge(&self, max_wait: Duration) -> bool {
    self
      .maybe_purge
      .as_ref()
      .is_none_or(|purge| purge.wait_until_purged(max_wait))
  }

  /// A change of SQLite's data version with no new numbered commit is a checkpoint of the log by
  /// another connection (the purge's, issue #1684), which changes no record: it doesn't count as
  /// another connection's commit, which would drop the caches and save the whole MLS state.
  fn write_tally(&self) -> Option<WriteTally> {
    let mut inner = self.lock();
    let Inner {
      connection,
      group,
      maybe_known_stamp,
      ..
    } = &mut *inner;
    // The known number moves only here, when the data version moved, and with this connection's
    // own commits: a commit between the two reads then still counts at the next look.
    Some(group.tally_checking(connection, || {
      let maybe_now = stamp_of(connection);
      let is_commit = maybe_now.is_none() || maybe_now != *maybe_known_stamp;
      *maybe_known_stamp = maybe_now;
      is_commit
    }))
  }

  fn stage_commit(&self) -> Option<CommitStamp> {
    let mut inner = self.lock();
    if !inner.group.will_commit(&inner.connection) {
      return None;
    }
    let writer = i64::from_ne_bytes(inner.writer.to_ne_bytes());
    // A store without the table (opened by the extension before the app made it) numbers nothing:
    // its reader then drops everything on a commit.
    let sequence: i64 = inner
      .connection
      .query_row(
        "UPDATE commit_stamp SET sequence = sequence + 1, writer = ?1 WHERE id = 0 RETURNING sequence",
        params![writer],
        |row| row.get(0),
      )
      .ok()?;
    let stamp = CommitStamp {
      sequence: u64::try_from(sequence).ok()?,
      writer: inner.writer,
    };
    inner.maybe_staged_stamp = Some(stamp);
    Some(stamp)
  }

  fn commit_stamp(&self) -> Option<CommitStamp> {
    stamp_of(&self.lock().connection)
  }
}

/// The number of the last commit to the database `connection` sees; `None` without the table.
fn stamp_of(connection: &Connection) -> Option<CommitStamp> {
  let (sequence, writer): (i64, i64) = connection
    .prepare_cached("SELECT sequence, writer FROM commit_stamp WHERE id = 0")
    .and_then(|mut statement| statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?))))
    .ok()?;
  Some(CommitStamp {
    sequence: u64::try_from(sequence).ok()?,
    writer: u64::from_ne_bytes(writer.to_ne_bytes()),
  })
}

impl Drop for SqliteRecords {
  fn drop(&mut self) {
    // Closing the app is the last chance to clear what the interval kept back: the purge thread
    // tries once more before it stops. A transaction left open counts as ended.
    let maybe_ticket = self.lock().maybe_ticket.take();
    if let (Some(purge), Some(ticket)) = (&self.maybe_purge, maybe_ticket) {
      purge.transaction_ended(ticket);
    }
    drop(self.maybe_purger.take());
  }
}

impl SqliteRecords {
  fn lock(&self) -> MutexGuard<'_, Inner> {
    // A poisoned lock only means another thread panicked; SQLite keeps the connection consistent.
    self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
  }

  /// Tells the purge whether the connection has a transaction open now (a read, or a group of
  /// writes): one that ends may be the moment the log can be truncated.
  fn note_transaction(&self, inner: &mut Inner) {
    let Some(purge) = &self.maybe_purge else {
      return;
    };
    let is_open = inner.is_reading || inner.group.is_open();
    match (is_open, inner.maybe_ticket) {
      (true, None) => inner.maybe_ticket = Some(purge.transaction_began()),
      (false, Some(ticket)) => {
        inner.maybe_ticket = None;
        purge.transaction_ended(ticket);
      },
      _ => {},
    }
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

#[cfg(test)]
mod tests {
  use rand_core::OsRng;
  use teamcraft_crypto::secret::SymmetricKey;

  use super::*;
  use crate::store::tests::{exercise_store, id, message};
  use crate::store::{LocalStore, StaticKeySource, Table};

  /// The helper test of the killed-build test, as the test binary names it.
  const KILLED_BUILD_HELPER: &str = "store::sealed_sqlite::tests::killed_build_helper";
  const HELPER_PATH: &str = "TEAMCRAFT_TEST_KILLED_BUILD_DATABASE";
  const HELPER_MARKER: &str = "TEAMCRAFT_TEST_KILLED_BUILD_MARKER";
  const HELPER_BATCH: &str = "TEAMCRAFT_TEST_KILLED_BUILD_BATCH";
  /// Entries and messages of the store the build is killed on: with the catalog's own records
  /// more than two batches.
  const ENTRIES: usize = 8_400;
  /// Longer than any purge that can run takes in a test.
  const PURGE_WAIT: Duration = Duration::from_secs(10);
  /// How long a test looks at a purge that can't run yet.
  const SHORT_WAIT: Duration = Duration::from_millis(300);
  const MESSAGES: u8 = 60;

  /// Issue #1466: a reader waiting for a group of writes is seen by the group's thread, and has its
  /// turn before the next group begins.
  #[test]
  fn a_waiting_reader_reads_between_two_groups() {
    let key = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
    let store = std::sync::Arc::new(SealedSqliteStore::open_in_memory(&key).unwrap());
    store.put(Table::Users, "1", b"before").unwrap();
    store.begin_writes().unwrap();
    assert!(!store.is_awaited(), "nobody waits yet");
    store.put(Table::Users, "1", b"in the first group").unwrap();
    let (answer, answered) = std::sync::mpsc::channel();
    let reader = {
      let store = store.clone();
      std::thread::spawn(move || answer.send(store.get(Table::Users, "1").unwrap()).unwrap())
    };
    while !store.is_awaited() {
      std::thread::yield_now();
    }
    store.end_writes().unwrap();
    store.begin_writes().unwrap();
    store.put(Table::Users, "1", b"in the second group").unwrap();
    store.end_writes().unwrap();
    // It had its turn between the two groups, not after both.
    let read = answered.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert_eq!(read.as_deref(), Some(&b"in the first group"[..]));
    reader.join().unwrap();
    assert!(!store.is_awaited());
  }

  /// Threads that read and write at once, some in groups that end early for the readers: every
  /// write lands, nobody waits for ever.
  #[test]
  fn readers_and_writers_in_groups_take_turns() {
    let key = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
    let store = std::sync::Arc::new(SealedSqliteStore::open_in_memory(&key).unwrap());
    let threads: Vec<_> = (0..4)
      .map(|thread| {
        let store = store.clone();
        std::thread::spawn(move || {
          for n in 0..200 {
            let key = format!("{thread}/{n}");
            if thread % 2 == 0 {
              crate::store::write_group(store.as_ref(), || {
                store.put(Table::Users, &key, b"written")?;
                if store.is_awaited() {
                  return Ok::<_, StoreError>(());
                }
                store.put(Table::Users, &format!("{key}/more"), b"written")
              })
              .unwrap();
            } else {
              crate::store::read_together(store.as_ref(), || store.get(Table::Users, "0/0").map(|_| ()))
                .unwrap();
            }
          }
        })
      })
      .collect();
    for thread in threads {
      thread.join().unwrap();
    }
    assert!(store.get(Table::Users, "2/199").unwrap().is_some());
    assert!(!store.is_awaited());
  }

  /// A read group holds no write lock: another connection (another process, the iOS notification
  /// extension) writes meanwhile, and the group's own writes still land.
  #[test]
  fn a_read_group_lets_another_connection_write() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
    let app = SealedSqliteStore::open(&path, &key).unwrap();
    let extension = SealedSqliteStore::open(&path, &key).unwrap();
    app.put(Table::Users, "1", b"before").unwrap();
    crate::store::read_together(&app, || {
      assert_eq!(app.get(Table::Users, "1")?.as_deref(), Some(&b"before"[..]));
      crate::store::write_group(&extension, || {
        extension.put(Table::Users, "2", b"from the extension")
      })?;
      crate::store::write_group(&app, || app.put(Table::Users, "3", b"from the app"))?;
      assert_eq!(app.get(Table::Users, "3")?.as_deref(), Some(&b"from the app"[..]));
      Ok::<_, StoreError>(())
    })
    .unwrap();
    assert_eq!(
      app.get(Table::Users, "2").unwrap().as_deref(),
      Some(&b"from the extension"[..])
    );
    assert_eq!(
      extension.get(Table::Users, "3").unwrap().as_deref(),
      Some(&b"from the app"[..])
    );
    drop((app, extension));
    let _ = std::fs::remove_dir_all(directory);
  }

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
    let store = open_purged(&path);
    store.put(Table::MlsStorage, "state", &[7; 64 * 1024]).unwrap();
    let needle = largest_record_part(&path);

    store.delete(Table::MlsStorage, "state").unwrap();
    store.purge_freed().unwrap();

    assert!(store.wait_for_purge(PURGE_WAIT));
    assert_files_lack(&path, &needle);
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #417: saving the MLS storage inside a group of writes clears the old snapshot from the
  /// log once the group is committed.
  #[test]
  fn a_purge_inside_a_group_waits_for_the_commit() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store = open_purged(&path);
    store.put(Table::MlsStorage, "state", &[7; 64 * 1024]).unwrap();
    let needle = largest_record_part(&path);

    store.begin_writes().unwrap();
    store.put(Table::MlsStorage, "state", &[9; 64 * 1024]).unwrap();
    store.purge_freed().unwrap();
    assert!(
      !store.wait_for_purge(SHORT_WAIT),
      "the open group keeps the purge waiting"
    );
    store.end_writes().unwrap();

    assert!(store.wait_for_purge(PURGE_WAIT));
    assert_files_lack(&path, &needle);
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #417: the MLS storage is saved for every message, so those purges are merged; an
  /// explicit purge, and closing the store, still clear the log.
  #[test]
  fn debounced_purges_are_merged_and_an_explicit_one_clears_what_is_left() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    // Opening purged the log: the next debounced purge waits for the interval.
    let store = open_purged(&path);
    store.put(Table::MlsStorage, "state", &[2; 64]).unwrap();
    store.purge_freed_soon();
    assert!(
      !store.wait_for_purge(SHORT_WAIT),
      "a purge within the interval waits"
    );
    store.purge_freed().unwrap();
    assert!(store.wait_for_purge(PURGE_WAIT));
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn closing_the_store_clears_the_log_of_a_waiting_purge() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store = open_purged(&path);
    store.put(Table::MlsStorage, "state", &[7; 64 * 1024]).unwrap();
    let needle = largest_record_part(&path);
    store.put(Table::MlsStorage, "state", &[9; 64 * 1024]).unwrap();
    store.purge_freed_soon();
    assert!(!store.wait_for_purge(SHORT_WAIT));
    drop(store);
    assert_files_lack(&path, &needle);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// A reader of another process (the iOS notification extension) holds the log: the purge can't
  /// finish, nothing waits for it, and it finishes once the reader is gone.
  #[test]
  fn a_purge_that_another_process_holds_up_finishes_after_it() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store = open_purged(&path);
    store.put(Table::MlsStorage, "state", &[1; 64]).unwrap();
    let reader = pin_log(&path);
    store.put(Table::MlsStorage, "state", &[2; 64]).unwrap();
    let started = std::time::Instant::now();
    store.purge_freed().unwrap();
    store.put(Table::MlsStorage, "state", &[3; 64]).unwrap();
    // TEAMCRAFT-F, #1678: the purge's tries held the writer.
    assert!(
      started.elapsed() < BUSY_TIMEOUT / 2,
      "neither the purge nor the next write waits for the reader"
    );
    assert!(!store.wait_for_purge(SHORT_WAIT), "the reader keeps the log");

    reader.execute_batch("COMMIT").unwrap();
    assert!(store.wait_for_purge(PURGE_WAIT), "it is tried again");
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #1684: the app crashed before the purge finished. The log it left still holds the
  /// deleted record's page; the next start clears it before it does anything else.
  #[test]
  fn a_purge_a_crash_cut_short_runs_when_the_store_opens_again() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = || StaticKeySource::new(SymmetricKey::from_bytes([9; 32]));
    let store = SealedSqliteStore::open(&path, &key()).unwrap();
    assert!(store.wait_for_purge(PURGE_WAIT));
    store.put(Table::MlsStorage, "state", &[7; 64 * 1024]).unwrap();
    let needle = largest_record_part(&path);
    let reader = pin_log(&path);
    store.delete(Table::MlsStorage, "state").unwrap();
    store.purge_freed().unwrap();
    // The files as a crash at this moment leaves them.
    let crashed = scratch_dir();
    let crashed_path = crashed.join("store.db");
    std::fs::copy(&path, &crashed_path).unwrap();
    std::fs::copy(
      path.with_extension("db-wal"),
      crashed_path.with_extension("db-wal"),
    )
    .unwrap();
    assert!(
      files_contain(&crashed_path, &needle),
      "the reader kept the deleted record in the files"
    );
    drop(reader);
    drop(store);

    let reopened = SealedSqliteStore::open(&crashed_path, &key()).unwrap();
    assert!(reopened.wait_for_purge(PURGE_WAIT));
    assert_files_lack(&crashed_path, &needle);
    drop(reopened);
    std::fs::remove_dir_all(&directory).unwrap();
    std::fs::remove_dir_all(&crashed).unwrap();
  }

  /// Issue #1684: the purge truncates on a connection of its own, which moves the writer's
  /// data version; that is no other connection's commit, which would drop the caches and make
  /// the next MLS save write everything.
  #[test]
  fn a_purge_does_not_count_as_another_connections_commit() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store = open_purged(&path);
    for round in 0..3_u8 {
      store.put(Table::MlsStorage, "state", &[round; 64]).unwrap();
      store.purge_freed().unwrap();
      assert!(store.wait_for_purge(PURGE_WAIT));
    }
    assert_eq!(store.write_tally(), Some(WriteTally::default()));

    let other = SealedSqliteStore::open(&path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng)));
    assert!(other.is_err(), "another key doesn't open it");
    let theirs = Connection::open(&path).unwrap();
    theirs
      .execute("UPDATE commit_stamp SET sequence = sequence + 1 WHERE id = 0", [])
      .unwrap();
    assert_eq!(
      store.write_tally().map(|tally| tally.foreign),
      Some(1),
      "another process's commit still counts"
    );
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #1681: the writer's commit no longer runs SQLite's automatic checkpoint (two syncs under
  /// its lock); a long log is written back on the purge's connection.
  #[test]
  fn a_long_log_is_written_back_off_the_writer() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let store = open_purged(&path);
    let autocheckpoint: i64 = store
      .backend()
      .lock()
      .connection
      .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
      .unwrap();
    assert_eq!(autocheckpoint, 0);
    crate::store::write_group(&store, || {
      for index in 0..80_u8 {
        store.put(Table::MlsStorage, &format!("entry {index}"), &[index; 64 * 1024])?;
      }
      Ok::<_, StoreError>(())
    })
    .unwrap();
    let purge = Arc::clone(store.backend().maybe_purge.as_ref().unwrap());
    assert!(purge.wait_for_backfill(PURGE_WAIT), "the commit asked for none");
    drop(store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// A store at `path` whose purge at opening is done.
  fn open_purged(path: &Path) -> SealedSqliteStore {
    let store =
      SealedSqliteStore::open(path, &StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    assert!(store.wait_for_purge(PURGE_WAIT));
    store
  }

  /// A connection of "another process" in the middle of a read transaction: it pins the log.
  fn pin_log(path: &Path) -> Connection {
    let reader = Connection::open(path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    reader
      .query_row("SELECT count(*) FROM sealed_records", [], |row| {
        row.get::<_, i64>(0)
      })
      .unwrap();
    reader
  }

  /// 32 bytes from the middle of the largest sealed record.
  fn largest_record_part(path: &Path) -> Vec<u8> {
    let sealed: Vec<u8> = Connection::open(path)
      .unwrap()
      .query_row(
        "SELECT value FROM sealed_records ORDER BY length(value) DESC LIMIT 1",
        [],
        |row| row.get(0),
      )
      .unwrap();
    sealed[sealed.len() / 2..sealed.len() / 2 + 32].to_vec()
  }

  fn files_contain(path: &Path, needle: &[u8]) -> bool {
    let raw = std::fs::read(path).unwrap();
    let wal = std::fs::read(path.with_extension("db-wal")).unwrap_or_default();
    [&raw, &wal]
      .iter()
      .any(|bytes| bytes.windows(needle.len()).any(|window| window == needle))
  }

  fn assert_files_lack(path: &Path, needle: &[u8]) {
    assert!(
      !files_contain(path, needle),
      "the deleted record is still in the files"
    );
  }

  /// The sync loop's group of writes on one thread, the app's reads and writes on another: they
  /// share the connection, so the reader must neither see the group's records nor write into it.
  mod groups_of_another_thread {
    use std::sync::mpsc;
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn a_read_waits_for_the_commit_and_sees_it() {
      let (store, directory) = shared_store();
      let in_group = Arc::new(Barrier::new(2));
      let (reader, answer) = read_after(&store, &in_group);
      store.begin_writes().unwrap();
      store.put(Table::Users, "key", b"new").unwrap();
      in_group.wait();
      // The reader may be anywhere up to its wait; it can't get past before the group ended.
      assert!(answer.try_recv().is_err(), "no answer inside the group");
      store.end_writes().unwrap();
      assert_eq!(answer.recv().unwrap(), Some(b"new".to_vec()));
      reader.join().unwrap();
      finish(store, directory);
    }

    #[test]
    fn a_read_never_sees_what_a_failed_group_rolled_back() {
      let (store, directory) = shared_store();
      store.put(Table::Users, "key", b"old").unwrap();
      let in_group = Arc::new(Barrier::new(2));
      let (reader, answer) = read_after(&store, &in_group);
      store.begin_writes().unwrap();
      store.put(Table::Users, "key", b"new").unwrap();
      in_group.wait();
      store.backend().fail_group();
      store.end_writes().unwrap_err();
      assert_eq!(answer.recv().unwrap(), Some(b"old".to_vec()));
      reader.join().unwrap();
      finish(store, directory);
    }

    #[test]
    fn a_failed_group_takes_its_feed_version_along() {
      let (store, directory) = shared_store();
      let conversation = id(10);
      let row = message(conversation, 1, Some(10), None, 1);
      store.upsert_message(&row).unwrap();
      let before = store.feed_version(conversation).unwrap();
      store.begin_writes().unwrap();
      store
        .upsert_message(&crate::store::StoredMessage {
          is_edited: true,
          ..row
        })
        .unwrap();
      assert_ne!(store.feed_version(conversation).unwrap(), before);
      store.backend().fail_group();
      store.end_writes().unwrap_err();
      assert_eq!(store.feed_version(conversation).unwrap(), before);
      finish(store, directory);
    }

    #[test]
    fn a_write_stays_out_of_the_group_and_survives_its_rollback() {
      let (store, directory) = shared_store();
      let in_group = Arc::new(Barrier::new(2));
      let writer = {
        let store = store.clone();
        let in_group = in_group.clone();
        std::thread::spawn(move || {
          in_group.wait();
          store.put(Table::Users, "other_thread", b"sent").unwrap();
        })
      };
      store.begin_writes().unwrap();
      store.put(Table::Users, "group", b"lost").unwrap();
      in_group.wait();
      store.backend().fail_group();
      store.end_writes().unwrap_err();
      writer.join().unwrap();
      assert_eq!(store.get(Table::Users, "group").unwrap(), None);
      assert_eq!(
        store.get(Table::Users, "other_thread").unwrap(),
        Some(b"sent".to_vec())
      );
      finish(store, directory);
    }

    #[test]
    fn nested_groups_keep_the_others_out_until_the_outermost_ends() {
      let (store, directory) = shared_store();
      let in_group = Arc::new(Barrier::new(2));
      let (reader, answer) = read_after(&store, &in_group);
      store.begin_writes().unwrap();
      store.begin_writes().unwrap();
      store.put(Table::Users, "key", b"new").unwrap();
      store.end_writes().unwrap();
      in_group.wait();
      assert!(answer.try_recv().is_err(), "the outer group is still open");
      store.end_writes().unwrap();
      assert_eq!(answer.recv().unwrap(), Some(b"new".to_vec()));
      reader.join().unwrap();
      finish(store, directory);
    }

    fn shared_store() -> (Arc<SealedSqliteStore>, std::path::PathBuf) {
      let directory = scratch_dir();
      let key = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
      let store = SealedSqliteStore::open(&directory.join("store.db"), &key).unwrap();
      (Arc::new(store), directory)
    }

    /// Reads `key` on another thread once `in_group` releases it, and answers with what it read.
    fn read_after(
      store: &Arc<SealedSqliteStore>,
      in_group: &Arc<Barrier>,
    ) -> (std::thread::JoinHandle<()>, mpsc::Receiver<Option<Vec<u8>>>) {
      let (answer, answered) = mpsc::channel();
      let store = store.clone();
      let in_group = in_group.clone();
      let reader = std::thread::spawn(move || {
        in_group.wait();
        answer.send(store.get(Table::Users, "key").unwrap()).unwrap();
      });
      (reader, answered)
    }

    fn finish(store: Arc<SealedSqliteStore>, directory: std::path::PathBuf) {
      drop(store);
      std::fs::remove_dir_all(&directory).unwrap();
    }
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
    let set = |key: &[u8], maybe_value: Option<&[u8]>| match maybe_value {
      Some(value) => provider.storage().set_value(key.to_vec(), value.to_vec()),
      None => provider.storage().remove_value(key),
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
      match maybe_value {
        Some(value) => provider.storage().set_value(key.to_vec(), value.to_vec()),
        None => provider.storage().remove_value(key),
      }
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
      provider.storage().set_value(key.to_vec(), value.to_vec());
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

  /// Issue #840: another process's commit of other records (the iOS notification extension's push
  /// bookkeeping, on every app resume) leaves the MLS state alone: the app's next save writes what
  /// changed, not the whole state again.
  #[test]
  fn another_process_committing_other_records_costs_no_rewrite_of_the_mls_state() {
    use openmls_traits::OpenMlsProvider as _;

    use crate::mls_storage::{SavedMlsStorage, load_mls_storage, save_mls_storage};

    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let app_store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
    let provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
    for entry in 0..50u8 {
      provider.storage().set_value(vec![entry], vec![entry; 1_000]);
    }
    save_mls_storage(&app_store, &provider, &SavedMlsStorage::default()).unwrap();
    drop(provider);
    let (provider, saved) = load_mls_storage(&app_store).unwrap();

    let extension = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    extension.put(Table::Users, "pushed", b"bookkeeping").unwrap();
    drop(extension);
    provider.storage().set_value(vec![0], vec![1; 1_000]);
    save_mls_storage(&app_store, &provider, &saved).unwrap();

    assert!(
      saved.written_bytes() < 2_000,
      "{} bytes written for one changed entry",
      saved.written_bytes()
    );
    drop(app_store);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #424 review: a rewrite after another writer's save also goes for the entries only that
  /// writer created, which this process never heard of: its cache follows the disk first.
  #[test]
  fn a_rewrite_leaves_no_entry_of_another_writer_behind() {
    use openmls_traits::OpenMlsProvider as _;

    use crate::mls_storage::{SavedMlsStorage, load_mls_storage, save_mls_storage};

    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let app_store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
    let provider = openmls_rust_crypto::OpenMlsRustCrypto::default();
    provider
      .storage()
      .set_value(b"X/context".to_vec(), b"epoch 1".to_vec());
    let saved = SavedMlsStorage::default();
    save_mls_storage(&app_store, &provider, &saved).unwrap();
    {
      let other_store = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate())).unwrap();
      let (other, other_saved) = load_mls_storage(&other_store).unwrap();
      other
        .storage()
        .set_value(b"X/only the other's".to_vec(), b"epoch 2".to_vec());
      save_mls_storage(&other_store, &other, &other_saved).unwrap();
    }
    provider
      .storage()
      .set_value(b"X/context".to_vec(), b"epoch 1, later".to_vec());
    save_mls_storage(&app_store, &provider, &saved).unwrap();
    drop(app_store);

    let reopened = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    let (loaded, _) = load_mls_storage(&reopened).unwrap();
    assert_eq!(
      *loaded.storage().values.read().unwrap(),
      *provider.storage().values.read().unwrap()
    );
    drop(reopened);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// A panic while the catalog is written, at the very end of a group: the group still ends. Left
  /// open at the backend, every later group would nest in it, report success and never commit, and
  /// the app would lose it all on exit though the server had seen those commits.
  #[test]
  fn a_panic_while_the_catalog_flushes_ends_the_group_and_rolls_it_back() {
    let connection = Connection::open_in_memory().unwrap();
    let backend = PanicsOnPut {
      records: SealedSqliteStore::backend_of(connection, Purging::None).unwrap(),
      is_armed: std::cell::Cell::new(false),
    };
    let key = SymmetricKey::generate(&mut OsRng);
    let store = SealedStore::open_backend(backend, &key, SealedStoreLimits::default()).unwrap();
    store.put(Table::Spaces, "before", b"1").unwrap();

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      crate::store::write_group(&store, || -> Result<(), StoreError> {
        store.put(Table::Spaces, "half", b"2")?;
        store.backend().is_armed.set(true);
        Ok(())
      })
    }));

    assert!(panicked.is_err(), "the panic goes on");
    assert!(
      !store.backend().is_armed.get(),
      "it was the catalog's write that panicked"
    );
    assert!(
      store.backend().records.lock().connection.is_autocommit(),
      "the transaction ended"
    );
    assert!(!store.has_group_owner());
    assert_eq!(store.write_tally().map(|tally| tally.lost), Some(1));
    assert_eq!(store.get(Table::Spaces, "half").unwrap(), None);
    assert_eq!(store.get(Table::Spaces, "before").unwrap(), Some(b"1".to_vec()));

    store.put(Table::Spaces, "after", b"3").unwrap();
    assert!(
      store.backend().records.lock().connection.is_autocommit(),
      "a later write commits"
    );
    assert_eq!(store.get(Table::Spaces, "after").unwrap(), Some(b"3".to_vec()));
  }

  /// Issue #1201: the panic of the catalog flush went past the tracked store's message cache, which
  /// kept the messages of the rolled-back group; a client reopened on the same store (the slot
  /// reopens it after a panic) showed them until a catch-up wrote them again.
  #[test]
  fn a_panic_while_the_catalog_flushes_leaves_no_rolled_back_message_in_the_cache() {
    use crate::store::{MessageFilter, TrackedStore};

    let connection = Connection::open_in_memory().unwrap();
    let backend = PanicsOnPut {
      records: SealedSqliteStore::backend_of(connection, Purging::None).unwrap(),
      is_armed: std::cell::Cell::new(false),
    };
    let key = SymmetricKey::generate(&mut OsRng);
    let sealed = SealedStore::open_backend(backend, &key, SealedStoreLimits::default()).unwrap();
    let store = TrackedStore::with_message_cache(std::rc::Rc::new(sealed), 1 << 20);
    let conversation = id(7);
    store
      .upsert_message(&message(conversation, 1, Some(100), None, 1))
      .unwrap();
    let clients = |store: &TrackedStore<_>| -> Vec<u8> {
      store
        .messages(conversation, MessageFilter::All)
        .unwrap()
        .iter()
        .map(|message| message.client_message_id.as_bytes()[0])
        .collect()
    };
    assert_eq!(clients(&store), [1], "read and kept");

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
      crate::store::write_group(&store, || -> Result<(), StoreError> {
        store.upsert_message(&message(conversation, 2, Some(200), None, 2))?;
        store.inner().backend().is_armed.set(true);
        Ok(())
      })
    }));
    assert!(panicked.is_err(), "the panic goes on");
    assert!(
      !store.inner().backend().is_armed.get(),
      "it was the catalog's write that panicked"
    );

    let reopened = store.clone();
    assert_eq!(clients(&reopened), [1], "the rolled-back message is gone");
    assert_eq!(
      clients(&reopened),
      clients(&TrackedStore::new(store.inner().clone())),
      "the cache says what the disk says"
    );
  }

  #[test]
  fn a_panic_undoes_its_group() {
    let store =
      SealedSqliteStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    crate::store::tests::a_panic_undoes_its_group(&store);
  }

  /// Issue #424 review, H1: 1.0.8 (its steps end before the marker) refuses the database; so does
  /// a version before the catalog or its feed indexes (issue #155), which would write records they
  /// don't list.
  #[test]
  fn versions_before_the_last_marker_refuse_the_database() {
    let store =
      SealedSqliteStore::open_in_memory(&StaticKeySource::new(SymmetricKey::generate(&mut OsRng))).unwrap();
    for steps in [2, 3, 4] {
      assert!(matches!(
        run_migrations(&store.backend().lock().connection, &MIGRATIONS[..steps]),
        Err(StoreError::NewerSchema { found: 5, supported })
          if supported == i64::try_from(steps).unwrap()
      ));
    }
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
    // One record damaged, the others fine: not a wrong key but a damaged record, reported when
    // it is read (issue #155: opening reads none of them).
    let damaged_id = SealedSqliteStore::open(&path, &StaticKeySource::new(key.duplicate()))
      .unwrap()
      .entry_id(Table::Users.name(), "1");
    connection
      .execute(
        "UPDATE sealed_records SET value = x'00' WHERE id = ?1",
        params![damaged_id.0.as_slice()],
      )
      .unwrap();
    drop(connection);
    let damaged = SealedSqliteStore::open(&path, &StaticKeySource::new(key)).unwrap();
    assert!(matches!(
      damaged.get(Table::Users, "1"),
      Err(StoreError::Corrupt(_))
    ));
    assert_eq!(damaged.get(Table::Users, "2").unwrap(), Some(b"two".to_vec()));
    drop(damaged);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #155 review: the extension's commit lands while this process waits for the write lock,
  /// after it last looked. The catalog nodes it holds are stale then; writing them would drop what
  /// the other process listed.
  #[test]
  fn a_commit_met_while_waiting_for_the_write_lock_drops_the_caches() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let key = SymmetricKey::generate(&mut OsRng);
    let source = StaticKeySource::new(key.duplicate());
    let conversation = id(7);
    let other = SealedSqliteStore::open(&path, &source).unwrap();
    let records = SealedSqliteStore::backend_of(Connection::open(&path).unwrap(), Purging::None).unwrap();
    let store = SealedStore::open_backend(
      Waiting {
        records,
        on_begin: std::cell::RefCell::new(None),
      },
      &key,
      SealedStoreLimits::default(),
    )
    .unwrap();
    store
      .upsert_message(&message(conversation, 1, Some(10), None, 1))
      .unwrap();
    assert_eq!(listed(&store, conversation), [1]);

    *store.backend().on_begin.borrow_mut() = Some(Box::new(|| {
      other
        .upsert_message(&message(conversation, 2, Some(20), None, 2))
        .unwrap();
    }));
    store
      .upsert_message(&message(conversation, 3, Some(30), None, 3))
      .unwrap();

    assert_eq!(listed(&store, conversation), [1, 2, 3]);
    drop(store);
    let reopened = SealedSqliteStore::open(&path, &source).unwrap();
    assert_eq!(listed(&reopened, conversation), [1, 2, 3], "the disk's index too");
    drop((reopened, other));
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// What another connection wrote, nodes rewritten and split, is read here at once.
  #[test]
  fn another_connections_writes_reach_the_caches() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let source = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
    let conversation = id(7);
    let mine = SealedSqliteStore::open(&path, &source).unwrap();
    let other = SealedSqliteStore::open(&path, &source).unwrap();
    mine
      .upsert_message(&message(conversation, 0, Some(1), None, 0))
      .unwrap();
    mine.put(Table::Users, "1", b"before").unwrap();
    assert_eq!(listed(&mine, conversation).len(), 1);

    crate::store::write_group(&other, || {
      for n in 1..=255u8 {
        other.upsert_message(&message(conversation, n, Some(i64::from(n) + 1), None, 0))?;
      }
      other.put(Table::Users, "1", b"after")
    })
    .unwrap();

    assert_eq!(listed(&mine, conversation).len(), 256);
    assert_eq!(mine.get(Table::Users, "1").unwrap(), Some(b"after".to_vec()));
    drop((mine, other));
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #1115: a commit of the iOS extension can't land inside a group of writes of the app:
  /// SQLite's write lock holds it off, so the app's unfinished group is neither mixed with it nor
  /// dropped from the caches, and both writes are there afterwards.
  #[test]
  fn an_open_group_of_writes_holds_another_connection_off_and_neither_is_lost() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let source = StaticKeySource::new(SymmetricKey::generate(&mut OsRng));
    let conversation = id(7);
    let app = SealedSqliteStore::open(&path, &source).unwrap();
    let extension = SealedSqliteStore::open(&path, &source).unwrap();

    app.begin_writes().unwrap();
    app
      .upsert_message(&message(conversation, 0, Some(1), None, 0))
      .unwrap();
    assert!(
      extension.put(Table::Users, "1", b"pushed").is_err(),
      "the app holds the write lock"
    );
    assert_eq!(
      listed(&app, conversation).len(),
      1,
      "its own write is still there"
    );
    app.end_writes().unwrap();

    extension.put(Table::Users, "1", b"pushed").unwrap();

    assert_eq!(app.get(Table::Users, "1").unwrap(), Some(b"pushed".to_vec()));
    assert_eq!(listed(&app, conversation).len(), 1);
    drop((app, extension));
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// The extension opens with a store that was never migrated: no build, the messages stay as they
  /// are for the app's build, and its point reads work.
  #[test]
  fn the_extension_opens_a_store_without_a_catalog_and_builds_none() {
    let directory = scratch_dir();
    let copy = directory.join("store.db");
    std::fs::copy(
      std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/store/v1.0.9-sealed.db"),
      &copy,
    )
    .unwrap();
    let source = StaticKeySource::new(SymmetricKey::from_bytes([0x5a; 32]));
    let count = |path: &std::path::Path| -> i64 {
      Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM sealed_records", [], |row| row.get(0))
        .unwrap()
    };
    let before = count(&copy);
    let store =
      SealedSqliteStore::open_without_building(&copy, &source, SealedStoreLimits::minimal()).unwrap();
    assert!(crate::records::load_account(&store).unwrap().is_some());
    assert_eq!(
      store
        .message_by_server_id(id(70), id(900))
        .unwrap()
        .map(|found| found.sent_at_ms),
      None,
      "not found, so it is fetched"
    );
    drop(store);
    assert_eq!(count(&copy), before);

    let app = SealedSqliteStore::open(&copy, &source).unwrap();
    assert!(app.message_by_server_id(id(70), id(900)).unwrap().is_some());
    drop(app);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// The database file and what its log holds, to compare before and after.
  fn files_of(path: &std::path::Path) -> (Vec<u8>, Vec<u8>) {
    let log = std::fs::read(path.with_extension("db-wal")).unwrap_or_default();
    (std::fs::read(path).unwrap(), log)
  }

  fn user_version(path: &std::path::Path) -> i64 {
    Connection::open(path)
      .unwrap()
      .query_row("PRAGMA user_version", [], |row| row.get(0))
      .unwrap()
  }

  fn copy_of_fixture(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let directory = scratch_dir();
    let copy = directory.join("store.db");
    std::fs::copy(
      std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/store")
        .join(name),
      &copy,
    )
    .unwrap();
    (directory, copy)
  }

  fn fixture_key() -> StaticKeySource {
    StaticKeySource::new(helper_key())
  }

  /// Issue #1198: a store of a release before the layout the extension can read is left exactly as
  /// it was, the refusal is one the extension answers with the generic notification (not a damaged
  /// store), and the app migrates it as ever on its next start.
  #[test]
  fn the_extension_leaves_an_old_store_as_it_is_for_the_app_to_migrate() {
    let (directory, path) = copy_of_fixture("v1.0.2-sealed.db");
    let before = files_of(&path);

    let refused =
      SealedSqliteStore::open_without_building(&path, &fixture_key(), SealedStoreLimits::minimal());
    let Err(error) = refused else {
      panic!("an old store must not open in the extension");
    };
    assert!(matches!(
      error,
      StoreError::LayoutBehind {
        found: 0,
        supported: 5
      }
    ));
    assert!(!error.is_unreadable(), "not a store to offer resetting");
    assert_eq!(files_of(&path), before, "nothing changed on disk");

    let app = SealedSqliteStore::open(&path, &fixture_key()).unwrap();
    assert!(crate::records::load_account(&app).unwrap().is_some());
    assert!(app.message_by_server_id(id(70), id(900)).unwrap().is_some());
    drop(app);
    assert_eq!(user_version(&path), 5);
    let reopened =
      SealedSqliteStore::open_without_building(&path, &fixture_key(), SealedStoreLimits::minimal());
    assert!(reopened.is_ok(), "the extension reads what the app migrated");
    drop(reopened);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// A store of the layout before the free-page wipe (`VACUUM`), the heaviest step there is: the
  /// extension doesn't run it, and the app does.
  #[test]
  fn the_extension_does_not_vacuum_a_store_that_is_due() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    let free_pages = |path: &std::path::Path| -> i64 {
      Connection::open(path)
        .unwrap()
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .unwrap()
    };
    {
      let connection = Connection::open(&path).unwrap();
      run_migrations(&connection, &MIGRATIONS[..1]).unwrap();
      connection
        .execute(
          "INSERT INTO sealed_records (id, value) VALUES (?1, ?2)",
          params![[1u8; LOOKUP_ID_LEN].as_slice(), [7u8; 64 * 1024].as_slice()],
        )
        .unwrap();
      connection.execute("DELETE FROM sealed_records", []).unwrap();
    }
    assert!(free_pages(&path) > 0);
    let before = files_of(&path);

    assert!(matches!(
      SealedSqliteStore::open_without_building(&path, &fixture_key(), SealedStoreLimits::minimal()),
      Err(StoreError::LayoutBehind { found: 1, .. })
    ));
    assert_eq!(files_of(&path), before);

    drop(SealedSqliteStore::open(&path, &fixture_key()).unwrap());
    assert_eq!(free_pages(&path), 0, "the app wiped them");
    assert_eq!(user_version(&path), 5);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Markers change no table, so a store with only those ahead still shows notifications; the
  /// extension leaves them, and the catalog's build, to the app.
  #[test]
  fn the_extension_reads_a_store_with_only_markers_ahead_and_moves_none() {
    let (directory, path) = copy_of_fixture("v1.0.9-sealed.db");
    assert_eq!(user_version(&path), 3);
    let store =
      SealedSqliteStore::open_without_building(&path, &fixture_key(), SealedStoreLimits::minimal()).unwrap();
    assert!(crate::records::load_account(&store).unwrap().is_some());
    drop(store);
    assert_eq!(user_version(&path), 3, "the markers stay for the app");

    drop(SealedSqliteStore::open(&path, &fixture_key()).unwrap());
    assert_eq!(user_version(&path), 5);
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Upkeep a failed `VACUUM` left due would run on every open: not in the extension.
  #[test]
  fn the_extension_leaves_upkeep_that_is_due_to_the_app() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    drop(SealedSqliteStore::open(&path, &fixture_key()).unwrap());
    let due = |path: &std::path::Path| -> i64 {
      Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM upkeep_due", [], |row| row.get(0))
        .unwrap()
    };
    Connection::open(&path)
      .unwrap()
      .execute_batch(
        "CREATE TABLE upkeep_due (version INTEGER PRIMARY KEY); INSERT INTO upkeep_due VALUES (2);",
      )
      .unwrap();

    drop(
      SealedSqliteStore::open_without_building(&path, &fixture_key(), SealedStoreLimits::minimal()).unwrap(),
    );
    assert_eq!(due(&path), 1, "the extension leaves it");

    drop(SealedSqliteStore::open(&path, &fixture_key()).unwrap());
    assert_eq!(due(&path), 0, "the app runs it");
    std::fs::remove_dir_all(&directory).unwrap();
  }

  #[test]
  fn the_extension_creates_no_store() {
    let directory = scratch_dir();
    let path = directory.join("store.db");
    assert!(matches!(
      SealedSqliteStore::open_without_building(&path, &fixture_key(), SealedStoreLimits::minimal()),
      Err(StoreError::Database(_))
    ));
    assert!(!path.exists());
    std::fs::remove_dir_all(&directory).unwrap();
  }

  /// Issue #1128: a process killed while a catalog build batch is written but not yet committed
  /// (the helper process dies at that point), however far the build had got, loses no record: the
  /// batch is rolled back, the next open goes on after the last committed batch, and the catalog
  /// is complete and agrees with the records.
  #[cfg(unix)]
  #[test]
  fn a_build_killed_before_a_batch_commits_loses_nothing() {
    use std::os::unix::process::ExitStatusExt as _;

    // Batches of 4,096 records: the build needs three, and dies in the first or the second.
    for dies_in_batch in [1, 2] {
      let directory = scratch_dir();
      let path = directory.join("store.db");
      write_store_without_catalog(&path);
      let marker = directory.join("died-at-the-marker");

      let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([KILLED_BUILD_HELPER, "--exact", "--nocapture", "--test-threads=1"])
        .env(HELPER_PATH, &path)
        .env(HELPER_MARKER, &marker)
        .env(HELPER_BATCH, dies_in_batch.to_string())
        .status()
        .unwrap();
      assert_eq!(
        status.signal(),
        Some(6),
        "the helper was aborted at its marked point: {status:?}"
      );
      assert!(marker.exists(), "it was the marked point that ended it");

      // Never a half batch: either nothing of the killed one is there, or all.
      let source = StaticKeySource::new(helper_key());
      let reopened = SealedSqliteStore::open(&path, &source).unwrap();
      assert_store_content(&reopened);
      drop(reopened);
      let again = SealedSqliteStore::open(&path, &source).unwrap();
      assert_store_content(&again);
      drop(again);
      std::fs::remove_dir_all(&directory).unwrap();
    }
  }

  /// The helper process of [`a_build_killed_before_a_batch_commits_loses_nothing`]: does nothing in
  /// an ordinary run. Run by that test, it builds the catalog and aborts the process when the
  /// numbered batch has written its records and is about to commit.
  #[test]
  fn killed_build_helper() {
    let Some(path) = std::env::var_os(HELPER_PATH) else {
      return;
    };
    let marker = std::env::var_os(HELPER_MARKER).unwrap();
    let batch: usize = std::env::var(HELPER_BATCH).unwrap().parse().unwrap();
    let records = SealedSqliteStore::backend_of(Connection::open(path).unwrap(), Purging::None).unwrap();
    let store = SealedStore::open_backend_to_build(
      Dying {
        records,
        commits_left: std::cell::Cell::new(batch),
        marker: marker.into(),
      },
      &helper_key(),
      SealedStoreLimits::default(),
    )
    .unwrap();
    let mut build = store.start_catalog_build().unwrap().unwrap();
    while store.step_catalog_build(&mut build).unwrap() != crate::store::sealed::BuildStep::Complete {}
    unreachable!("the build has more batches than the helper lives for");
  }

  /// Records on SQLite, running `on_begin` before the next group takes the write lock: what another
  /// process commits while this one waits for it.
  struct Waiting<'a> {
    records: SqliteRecords,
    on_begin: std::cell::RefCell<Option<Box<dyn FnOnce() + 'a>>>,
  }

  impl RecordBackend for Waiting<'_> {
    fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
      self.records.put_record(id, sealed)
    }

    fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
      self.records.delete_record(id)
    }

    fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
      self.records.get_record(id)
    }

    fn records_after(
      &self,
      maybe_after: Option<&LookupId>,
      limit: usize,
    ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
      self.records.records_after(maybe_after, limit)
    }

    fn begin_read(&self) -> Result<(), StoreError> {
      self.records.begin_read()
    }

    fn end_read(&self) {
      self.records.end_read();
    }

    fn begin_writes(&self) -> Result<(), StoreError> {
      if let Some(commit) = self.on_begin.borrow_mut().take() {
        commit();
      }
      self.records.begin_writes()
    }

    fn end_writes(&self) -> Result<(), StoreError> {
      self.records.end_writes()
    }

    fn fail_group(&self) {
      self.records.fail_group();
    }

    fn write_tally(&self) -> Option<WriteTally> {
      self.records.write_tally()
    }
  }

  /// Records on SQLite whose next put panics once armed.
  struct PanicsOnPut {
    records: SqliteRecords,
    is_armed: std::cell::Cell<bool>,
  }

  impl RecordBackend for PanicsOnPut {
    fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
      assert!(!self.is_armed.replace(false), "made to panic");
      self.records.put_record(id, sealed)
    }

    fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
      self.records.delete_record(id)
    }

    fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
      self.records.get_record(id)
    }

    fn records_after(
      &self,
      maybe_after: Option<&LookupId>,
      limit: usize,
    ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
      self.records.records_after(maybe_after, limit)
    }

    fn begin_writes(&self) -> Result<(), StoreError> {
      self.records.begin_writes()
    }

    fn end_writes(&self) -> Result<(), StoreError> {
      self.records.end_writes()
    }

    fn fail_group(&self) {
      self.records.fail_group();
    }

    fn write_tally(&self) -> Option<WriteTally> {
      self.records.write_tally()
    }
  }

  /// Records on SQLite that abort the process when the numbered commit is about to happen, after
  /// its writes: a kill in the middle of a transaction, at a point the test knows.
  struct Dying {
    records: SqliteRecords,
    commits_left: std::cell::Cell<usize>,
    marker: std::path::PathBuf,
  }

  impl RecordBackend for Dying {
    fn put_record(&self, id: &LookupId, sealed: &[u8]) -> Result<(), StoreError> {
      self.records.put_record(id, sealed)
    }

    fn delete_record(&self, id: &LookupId) -> Result<(), StoreError> {
      self.records.delete_record(id)
    }

    fn get_record(&self, id: &LookupId) -> Result<Option<Vec<u8>>, StoreError> {
      self.records.get_record(id)
    }

    fn records_after(
      &self,
      maybe_after: Option<&LookupId>,
      limit: usize,
    ) -> Result<Vec<(LookupId, Vec<u8>)>, StoreError> {
      self.records.records_after(maybe_after, limit)
    }

    fn begin_read(&self) -> Result<(), StoreError> {
      self.records.begin_read()
    }

    fn end_read(&self) {
      self.records.end_read();
    }

    fn begin_writes(&self) -> Result<(), StoreError> {
      self.records.begin_writes()
    }

    fn end_writes(&self) -> Result<(), StoreError> {
      self.commits_left.set(self.commits_left.get() - 1);
      if self.commits_left.get() == 0 {
        std::fs::write(&self.marker, b"died").unwrap();
        // The point of this helper process: gone with the transaction open.
        std::process::abort();
      }
      self.records.end_writes()
    }

    fn fail_group(&self) {
      self.records.fail_group();
    }

    fn write_tally(&self) -> Option<WriteTally> {
      self.records.write_tally()
    }
  }

  /// The records every killed build starts from: entries and messages, written the way a version
  /// before the catalog wrote them (no catalog yet), enough for three batches.
  fn write_store_without_catalog(path: &std::path::Path) {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(LOG_PRAGMAS).unwrap();
    run_migrations(&connection, MIGRATIONS).unwrap();
    drop(connection);
    let store = SealedSqliteStore::open_without_building(
      path,
      &StaticKeySource::new(helper_key()),
      SealedStoreLimits::default(),
    )
    .unwrap();
    store.begin_writes().unwrap();
    for index in 0..ENTRIES {
      store
        .put(
          Table::Users,
          &format!("user {index}"),
          format!("value {index}").as_bytes(),
        )
        .unwrap();
    }
    for client in 1..=MESSAGES {
      store
        .upsert_message(&message(
          id(7),
          client,
          Some(10 * i64::from(client)),
          None,
          i64::from(client),
        ))
        .unwrap();
    }
    store.end_writes().unwrap();
  }

  /// Every record `write_store_without_catalog` wrote is there, listed and read.
  fn assert_store_content(store: &SealedSqliteStore) {
    let users = store.list(Table::Users).unwrap();
    assert_eq!(users.len(), ENTRIES);
    for index in 0..ENTRIES {
      assert!(
        users.contains(&(format!("user {index}"), format!("value {index}").into_bytes())),
        "user {index}"
      );
    }
    assert_eq!(listed(store, id(7)), (1..=MESSAGES).collect::<Vec<u8>>());
  }

  fn helper_key() -> SymmetricKey {
    SymmetricKey::from_bytes([0x5a; 32])
  }

  fn listed(store: &impl LocalStore, conversation: teamcraft_proto::ConversationId) -> Vec<u8> {
    store
      .messages(conversation, crate::store::MessageFilter::All)
      .unwrap()
      .iter()
      .map(|stored| stored.client_message_id.as_bytes()[0])
      .collect()
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
