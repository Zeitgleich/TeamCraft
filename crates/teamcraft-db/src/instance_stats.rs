//! Numbers about the instance as a whole, for the metrics endpoint (operations.md, "Metrics"). They
//! hold no identifiers and no content. [`Database::instance_stats`] reads the disk's view of the
//! database, so it runs on a timer, never for a request or a scrape.

use crate::database::Database;
use crate::error::DbError;

/// What the periodic check reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceStats {
  pub database_size_bytes: i64,
  /// Wake-ups in the retry queue.
  pub push_retries_pending: i64,
}

/// The connection pool right now. In memory: reading it costs nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolStats {
  /// Connections open, idle ones included.
  pub size: u32,
  pub idle: u32,
  /// The most connections the pool opens (`[database] max_connections`).
  pub max: u32,
}

impl Database {
  /// The size of this database on disk and the length of the push retry queue.
  pub async fn instance_stats(&self) -> Result<InstanceStats, DbError> {
    let row = sqlx::query!(
      r#"SELECT pg_database_size(current_database()) AS "database_size_bytes!",
                (SELECT COUNT(*) FROM push_retries) AS "push_retries_pending!""#
    )
    .fetch_one(self.pool())
    .await?;
    Ok(InstanceStats {
      database_size_bytes: row.database_size_bytes,
      push_retries_pending: row.push_retries_pending,
    })
  }

  pub fn pool_stats(&self) -> PoolStats {
    let pool = self.pool();
    PoolStats {
      size: pool.size(),
      idle: u32::try_from(pool.num_idle()).unwrap_or(u32::MAX),
      max: pool.options().get_max_connections(),
    }
  }
}

#[cfg(all(test, feature = "integration"))]
mod tests {
  use crate::testing::TestDatabase;

  #[tokio::test]
  async fn the_check_reads_a_size_and_an_empty_queue() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let stats = test_db.db.instance_stats().await.unwrap();
    assert!(stats.database_size_bytes > 0);
    assert_eq!(stats.push_retries_pending, 0);
    test_db.drop_database().await;
  }

  #[tokio::test]
  async fn the_pool_reports_its_maximum() {
    let Some(test_db) = TestDatabase::create().await else {
      return;
    };
    let pool = test_db.db.pool_stats();
    assert!(pool.max >= 1 && pool.size <= pool.max && pool.idle <= pool.size);
    test_db.drop_database().await;
  }
}
