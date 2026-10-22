//! Throwaway databases for integration tests (feature `integration`).
//!
//! Tests skip themselves (with a note on stderr) when `DATABASE_URL` is not set, so
//! `cargo test --workspace --all-features` also works without the dev stack. Each test migrates its
//! own database on the server named by `DATABASE_URL` and drops it at the end.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{Connection, PgConnection};

/// Re-exported so tests of other crates can seed and inspect the database directly.
pub use sqlx::{self, PgPool};

use crate::{Database, DatabaseOptions};

static DATABASE_COUNTER: AtomicU32 = AtomicU32::new(0);

pub struct TestDatabase {
  pub db: Database,
  pub pool: PgPool,
  name: String,
  admin_url: String,
}

impl TestDatabase {
  /// `None` (the test is skipped) when `DATABASE_URL` is not set.
  pub async fn create() -> Option<Self> {
    let Ok(admin_url) = std::env::var("DATABASE_URL") else {
      eprintln!("skipping: DATABASE_URL is not set");
      return None;
    };
    let nanos = SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .unwrap()
      .subsec_nanos();
    let name = format!(
      "tc_test_{}_{}_{nanos}",
      std::process::id(),
      DATABASE_COUNTER.fetch_add(1, Ordering::SeqCst)
    );

    let mut admin = PgConnection::connect(&admin_url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {name}")))
      .execute(&mut admin)
      .await
      .unwrap();

    let (base, _) = admin_url.rsplit_once('/').unwrap();
    // The same connection settings as production (jit off).
    let db = Database::connect(&format!("{base}/{name}"), &DatabaseOptions::default())
      .await
      .unwrap();
    let pool = db.pool().clone();
    db.migrate().await.unwrap();
    Some(Self {
      db,
      pool,
      name,
      admin_url,
    })
  }

  pub async fn table_exists(&self, name: &str) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
      .bind(name)
      .fetch_one(&self.pool)
      .await
      .unwrap()
  }

  pub async fn drop_database(self) {
    self.pool.close().await;
    let mut admin = PgConnection::connect(&self.admin_url).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(format!(
      "DROP DATABASE IF EXISTS {} WITH (FORCE)",
      self.name
    )))
    .execute(&mut admin)
    .await
    .unwrap();
  }
}
