use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tracing::info;

use crate::error::DbError;

/// Tables partitioned by month on the Snowflake ID (migration 0001).
const PARTITIONED_TABLES: [&str; 2] = ["messages", "mls_messages"];

/// Handle to the Postgres pool. Cheap to clone.
#[derive(Clone)]
pub struct Database {
  pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct DatabaseOptions {
  pub max_connections: u32,
  pub acquire_timeout: Duration,
}

impl Default for DatabaseOptions {
  fn default() -> Self {
    Self {
      max_connections: 10,
      acquire_timeout: Duration::from_secs(5),
    }
  }
}

impl Database {
  /// Connects lazily-validated: fails if the server is unreachable.
  pub async fn connect(url: &str, options: &DatabaseOptions) -> Result<Self, DbError> {
    // JIT compilation costs tens to hundreds of milliseconds and only pays off for long analytical
    // queries; with it on, the first runs of our short queries on a partitioned table (whose plan
    // cost is high because of the partition count) spend all their time compiling. Found by the
    // load test (docs/performance.md).
    let connect_options = url.parse::<PgConnectOptions>()?.options([("jit", "off")]);
    let pool = PgPoolOptions::new()
      .max_connections(options.max_connections)
      .acquire_timeout(options.acquire_timeout)
      .connect_with(connect_options)
      .await?;
    Ok(Self::from_pool(pool))
  }

  pub fn from_pool(pool: PgPool) -> Self {
    Self { pool }
  }

  /// Applies pending forward-only migrations.
  pub async fn migrate(&self) -> Result<(), DbError> {
    sqlx::migrate!("./migrations").run(&self.pool).await?;
    Ok(())
  }

  /// Makes sure monthly partitions exist for the current month and `months_ahead` more.
  /// Idempotent; the server calls it on startup and periodically.
  pub async fn ensure_partitions(&self, months_ahead: u32) -> Result<(), DbError> {
    let months = i32::try_from(months_ahead.saturating_add(1)).unwrap_or(i32::MAX);
    for table in PARTITIONED_TABLES {
      sqlx::query!(
        "SELECT tc_ensure_month_partitions($1::text, now(), $2::int)",
        table,
        months
      )
      .execute(&self.pool)
      .await?;
      info!(table, months, "partitions ensured");
    }
    Ok(())
  }

  /// Cheap round trip for readiness checks.
  pub async fn ping(&self) -> Result<(), DbError> {
    sqlx::query!("SELECT 1 AS \"one!\"").fetch_one(&self.pool).await?;
    Ok(())
  }

  pub(crate) fn pool(&self) -> &PgPool {
    &self.pool
  }
}

#[cfg(test)]
mod tests {
  use std::collections::BTreeSet;

  // Two branches can each add "the next" migration; sqlx then fails at startup on the duplicate
  // version, and no test without a database would notice.
  #[test]
  fn migration_versions_are_unique() {
    let migrator = sqlx::migrate!("./migrations");
    let mut seen = BTreeSet::new();
    for migration in migrator.iter() {
      assert!(
        seen.insert(migration.version),
        "migration version {} is used twice ({})",
        migration.version,
        migration.description
      );
    }
  }
}
