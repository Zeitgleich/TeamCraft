//! The gateway's state: which servers exist (public key, block state, limit overrides) and daily
//! usage counters. SQLite, because the data is a few small tables owned by one process on one host
//! (docs/push-gateway.md §6). Hot-path state (rate limits, replay cache) is in memory and never
//! stored; tokens and wake-ups are never stored at all.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS servers (
  server_id         TEXT PRIMARY KEY,
  public_key        BLOB NOT NULL,
  created_at        INTEGER NOT NULL,
  last_seen_at      INTEGER NOT NULL,
  is_blocked        INTEGER NOT NULL DEFAULT 0,
  blocked_reason    TEXT,
  blocked_at        INTEGER,
  pushes_per_second INTEGER,
  burst             INTEGER,
  pushes_per_day    INTEGER
) STRICT;
CREATE TABLE IF NOT EXISTS usage (
  server_id TEXT NOT NULL,
  day       INTEGER NOT NULL,
  accepted  INTEGER NOT NULL DEFAULT 0,
  gone      INTEGER NOT NULL DEFAULT 0,
  failed    INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (server_id, day)
) STRICT;
";

/// Days of usage counters kept.
const USAGE_RETENTION_DAYS: i64 = 60;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
  #[error("database error: {0}")]
  Database(#[from] rusqlite::Error),
  #[error("the database task failed")]
  Task,
}

/// One registered server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerRecord {
  pub server_id: String,
  pub public_key: [u8; 32],
  pub created_at: i64,
  pub last_seen_at: i64,
  pub block: BlockState,
  pub limits: LimitOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockState {
  Active,
  Blocked { reason: String },
}

/// Per-server limits that replace the configured defaults; `None` keeps the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LimitOverrides {
  pub maybe_pushes_per_second: Option<u32>,
  pub maybe_burst: Option<u32>,
  pub maybe_pushes_per_day: Option<u64>,
}

/// Counters of one server and day.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageCounts {
  pub accepted: u64,
  pub gone: u64,
  pub failed: u64,
}

/// What `servers list` shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSummary {
  pub record: ServerRecord,
  pub today: UsageCounts,
}

#[derive(Clone)]
pub struct Store {
  connection: Arc<Mutex<Connection>>,
}

impl Store {
  /// Opens (and creates) the database; `:memory:` for tests.
  pub fn open(path: &Path) -> Result<Self, StoreError> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    // WAL lets the CLI read and block servers while the gateway runs.
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.execute_batch(SCHEMA)?;
    Ok(Self {
      connection: Arc::new(Mutex::new(connection)),
    })
  }

  pub async fn get_server(&self, server_id: &str) -> Result<Option<ServerRecord>, StoreError> {
    let server_id = server_id.to_owned();
    self.run(move |db| get_server(db, &server_id)).await
  }

  /// Adds a server; `false` if its ID exists already.
  pub async fn insert_server(
    &self,
    server_id: &str,
    public_key: [u8; 32],
    now: i64,
  ) -> Result<bool, StoreError> {
    let server_id = server_id.to_owned();
    self
      .run(move |db| {
        let changed = db.execute(
          "INSERT OR IGNORE INTO servers (server_id, public_key, created_at, last_seen_at) VALUES (?1, ?2, ?3, ?3)",
          params![server_id, public_key.as_slice(), now],
        )?;
        Ok(changed == 1)
      })
      .await
  }

  pub async fn count_servers(&self) -> Result<u64, StoreError> {
    self
      .run(|db| {
        Ok(
          db.query_row("SELECT COUNT(*) FROM servers", [], |row| row.get::<_, i64>(0))
            .map(as_u64)?,
        )
      })
      .await
  }

  /// Blocks or unblocks; `false` if the server is unknown.
  pub async fn set_blocked(
    &self,
    server_id: &str,
    maybe_reason: Option<&str>,
    now: i64,
  ) -> Result<bool, StoreError> {
    let server_id = server_id.to_owned();
    let maybe_reason = maybe_reason.map(str::to_owned);
    self
      .run(move |db| {
        let changed = match maybe_reason {
          Some(reason) => db.execute(
            "UPDATE servers SET is_blocked = 1, blocked_reason = ?2, blocked_at = ?3 WHERE server_id = ?1",
            params![server_id, reason, now],
          )?,
          None => db.execute(
            "UPDATE servers SET is_blocked = 0, blocked_reason = NULL, blocked_at = NULL WHERE server_id = ?1",
            params![server_id],
          )?,
        };
        Ok(changed == 1)
      })
      .await
  }

  pub async fn set_limits(&self, server_id: &str, limits: LimitOverrides) -> Result<bool, StoreError> {
    let server_id = server_id.to_owned();
    self
      .run(move |db| {
        let changed = db.execute(
          "UPDATE servers SET pushes_per_second = ?2, burst = ?3, pushes_per_day = ?4 WHERE server_id = ?1",
          params![
            server_id,
            limits.maybe_pushes_per_second,
            limits.maybe_burst,
            limits.maybe_pushes_per_day.map(as_i64)
          ],
        )?;
        Ok(changed == 1)
      })
      .await
  }

  /// Adds the counters of a flush interval and records activity.
  pub async fn add_usage(
    &self,
    entries: Vec<(String, UsageCounts)>,
    day: i64,
    now: i64,
  ) -> Result<(), StoreError> {
    self
      .run(move |db| {
        let transaction = db.unchecked_transaction()?;
        for (server_id, counts) in &entries {
          transaction.execute(
            "INSERT INTO usage (server_id, day, accepted, gone, failed) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (server_id, day) DO UPDATE SET
               accepted = accepted + excluded.accepted,
               gone = gone + excluded.gone,
               failed = failed + excluded.failed",
            params![
              server_id,
              day,
              as_i64(counts.accepted),
              as_i64(counts.gone),
              as_i64(counts.failed)
            ],
          )?;
          transaction.execute(
            "UPDATE servers SET last_seen_at = ?2 WHERE server_id = ?1",
            params![server_id, now],
          )?;
        }
        transaction.execute(
          "DELETE FROM usage WHERE day < ?1",
          params![day - USAGE_RETENTION_DAYS],
        )?;
        transaction.commit()?;
        Ok(())
      })
      .await
  }

  pub async fn usage(&self, server_id: &str, day: i64) -> Result<UsageCounts, StoreError> {
    let server_id = server_id.to_owned();
    self.run(move |db| usage(db, &server_id, day)).await
  }

  pub async fn list_servers(&self, day: i64) -> Result<Vec<ServerSummary>, StoreError> {
    self
      .run(move |db| {
        let mut statement = db.prepare(
          "SELECT server_id, public_key, created_at, last_seen_at, is_blocked, blocked_reason,
                  pushes_per_second, burst, pushes_per_day FROM servers ORDER BY created_at, server_id",
        )?;
        let records = statement
          .query_map([], row_to_record)?
          .collect::<Result<Vec<_>, _>>()?;
        records
          .into_iter()
          .map(|record| {
            let today = usage(db, &record.server_id, day)?;
            Ok(ServerSummary { record, today })
          })
          .collect()
      })
      .await
  }

  /// A cheap query for the health endpoint.
  pub async fn ping(&self) -> Result<(), StoreError> {
    self
      .run(|db| {
        Ok(
          db.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
            .map(drop)?,
        )
      })
      .await
  }

  async fn run<T: Send + 'static>(
    &self,
    work: impl FnOnce(&Connection) -> Result<T, StoreError> + Send + 'static,
  ) -> Result<T, StoreError> {
    let connection = self.connection.clone();
    tokio::task::spawn_blocking(move || work(&lock(&connection)))
      .await
      .map_err(|_| StoreError::Task)?
  }
}

fn get_server(db: &Connection, server_id: &str) -> Result<Option<ServerRecord>, StoreError> {
  Ok(
    db.query_row(
      "SELECT server_id, public_key, created_at, last_seen_at, is_blocked, blocked_reason,
              pushes_per_second, burst, pushes_per_day FROM servers WHERE server_id = ?1",
      params![server_id],
      row_to_record,
    )
    .optional()?,
  )
}

fn usage(db: &Connection, server_id: &str, day: i64) -> Result<UsageCounts, StoreError> {
  Ok(
    db.query_row(
      "SELECT accepted, gone, failed FROM usage WHERE server_id = ?1 AND day = ?2",
      params![server_id, day],
      |row| {
        Ok(UsageCounts {
          accepted: as_u64(row.get(0)?),
          gone: as_u64(row.get(1)?),
          failed: as_u64(row.get(2)?),
        })
      },
    )
    .optional()?
    .unwrap_or_default(),
  )
}

fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServerRecord> {
  let key: Vec<u8> = row.get(1)?;
  let public_key = <[u8; 32]>::try_from(key.as_slice())
    .map_err(|_| rusqlite::Error::InvalidColumnType(1, "public_key".into(), rusqlite::types::Type::Blob))?;
  let is_blocked: bool = row.get(4)?;
  Ok(ServerRecord {
    server_id: row.get(0)?,
    public_key,
    created_at: row.get(2)?,
    last_seen_at: row.get(3)?,
    block: if is_blocked {
      BlockState::Blocked {
        reason: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
      }
    } else {
      BlockState::Active
    },
    limits: LimitOverrides {
      maybe_pushes_per_second: row.get(6)?,
      maybe_burst: row.get(7)?,
      maybe_pushes_per_day: row.get::<_, Option<i64>>(8)?.map(as_u64),
    },
  })
}

/// SQLite integers are signed 64-bit; counters never get near the top.
fn as_i64(value: u64) -> i64 {
  i64::try_from(value).unwrap_or(i64::MAX)
}

fn as_u64(value: i64) -> u64 {
  u64::try_from(value).unwrap_or(0)
}

/// A poisoned lock means a panic mid-query; SQLite's own transactions keep the data consistent.
fn lock(connection: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
  connection.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
  use super::*;

  const KEY: [u8; 32] = [7; 32];

  #[tokio::test]
  async fn servers_are_added_once_and_blocked_and_unblocked() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    assert!(store.insert_server("aa", KEY, 10).await.unwrap());
    assert!(!store.insert_server("aa", [8; 32], 11).await.unwrap());
    let record = store.get_server("aa").await.unwrap().unwrap();
    assert_eq!(record.public_key, KEY);
    assert_eq!(record.block, BlockState::Active);

    assert!(store.set_blocked("aa", Some("abuse"), 12).await.unwrap());
    let blocked = store.get_server("aa").await.unwrap().unwrap();
    assert_eq!(
      blocked.block,
      BlockState::Blocked {
        reason: "abuse".into()
      }
    );
    assert!(store.set_blocked("aa", None, 13).await.unwrap());
    assert_eq!(
      store.get_server("aa").await.unwrap().unwrap().block,
      BlockState::Active
    );
    assert!(!store.set_blocked("missing", None, 13).await.unwrap());
  }

  #[tokio::test]
  async fn usage_adds_up_per_day_and_old_days_are_pruned() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.insert_server("aa", KEY, 0).await.unwrap();
    let counts = UsageCounts {
      accepted: 2,
      gone: 1,
      failed: 0,
    };
    store
      .add_usage(vec![("aa".into(), counts)], 100, 5)
      .await
      .unwrap();
    store
      .add_usage(vec![("aa".into(), counts)], 100, 6)
      .await
      .unwrap();
    assert_eq!(
      store.usage("aa", 100).await.unwrap(),
      UsageCounts {
        accepted: 4,
        gone: 2,
        failed: 0
      }
    );
    assert_eq!(store.usage("aa", 99).await.unwrap(), UsageCounts::default());
    store
      .add_usage(vec![], 100 + USAGE_RETENTION_DAYS + 1, 7)
      .await
      .unwrap();
    assert_eq!(store.usage("aa", 100).await.unwrap(), UsageCounts::default());
  }

  #[tokio::test]
  async fn limit_overrides_round_trip() {
    let store = Store::open(Path::new(":memory:")).unwrap();
    store.insert_server("aa", KEY, 0).await.unwrap();
    let limits = LimitOverrides {
      maybe_pushes_per_second: Some(5),
      maybe_burst: None,
      maybe_pushes_per_day: Some(9),
    };
    assert!(store.set_limits("aa", limits).await.unwrap());
    assert_eq!(store.get_server("aa").await.unwrap().unwrap().limits, limits);
    assert_eq!(store.list_servers(0).await.unwrap().len(), 1);
    store.ping().await.unwrap();
    assert_eq!(store.count_servers().await.unwrap(), 1);
  }
}
