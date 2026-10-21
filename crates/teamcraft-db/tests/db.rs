//! Integration tests against Postgres. They run only with `--features integration` and skip (with a
//! note on stderr) when `DATABASE_URL` is not set, so `cargo test --workspace --all-features` also
//! works without the dev stack. Start it with `docker compose -f docker-compose.dev.yml up -d postgres`.
//! Every test migrates its own throwaway database on that server.
#![cfg(feature = "integration")]
// Test helpers outside `#[test]` functions are not covered by clippy.toml.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stderr)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{Connection, PgConnection, PgPool};
use teamcraft_db::{Database, OidcIdentity, SnowflakeGenerator, UserKind};
use teamcraft_proto::UserId;

const ISSUER: &str = "https://auth.example.com/realms/dev";

static DATABASE_COUNTER: AtomicU32 = AtomicU32::new(0);

#[tokio::test]
async fn upsert_creates_then_updates_user() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let identity = identity("sub-1", "Alice");

  let first = db
    .upsert_oidc_user(&identity, ids.next_id().unwrap())
    .await
    .unwrap();
  let renamed = OidcIdentity {
    display_name: "Alice B.",
    kind: UserKind::Guest,
    ..identity
  };
  let second = db
    .upsert_oidc_user(&renamed, ids.next_id::<UserId>().unwrap())
    .await
    .unwrap();

  assert_eq!(first.id, second.id);
  assert_eq!(second.display_name, "Alice B.");
  assert_eq!(second.kind, UserKind::Guest);
  test_db.drop_database().await;
}

#[tokio::test]
async fn sync_keeps_existing_user_and_applies_changes() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let db = &test_db.db;
  let ids = SnowflakeGenerator::new(1).unwrap();
  let identity = identity("sub-2", "Bob");

  let first = db
    .sync_oidc_user(&identity, ids.next_id().unwrap())
    .await
    .unwrap();
  let again = db
    .sync_oidc_user(&identity, ids.next_id().unwrap())
    .await
    .unwrap();
  assert_eq!(first.id, again.id);

  let renamed = OidcIdentity {
    display_name: "Robert",
    ..identity
  };
  let updated = db.sync_oidc_user(&renamed, ids.next_id().unwrap()).await.unwrap();
  assert_eq!((updated.id, updated.display_name.as_str()), (first.id, "Robert"));
  test_db.drop_database().await;
}

#[tokio::test]
async fn partitions_are_created_idempotently() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  test_db.db.ensure_partitions(2).await.unwrap();
  test_db.db.ensure_partitions(2).await.unwrap();

  for table in ["messages", "mls_messages"] {
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_inherits WHERE inhparent = $1::regclass")
      .bind(table)
      .fetch_one(&test_db.pool)
      .await
      .unwrap();
    // Default partition plus the months created by the migration and by the calls above.
    assert!(count >= 4, "unexpected partition count {count} for {table}");
  }
  test_db.drop_database().await;
}

#[tokio::test]
async fn rows_land_in_the_partition_of_their_month() {
  let Some(test_db) = TestDatabase::create().await else {
    return;
  };
  let pool = &test_db.pool;
  sqlx::query("INSERT INTO users (id, display_name, kind) VALUES (1, 'A', 'member')")
    .execute(pool)
    .await
    .unwrap();
  sqlx::query("INSERT INTO devices (id, user_id, name, public_key, cross_signature) VALUES (1, 1, 'd', '\\x00', '\\x00')")
    .execute(pool)
    .await
    .unwrap();
  sqlx::query("INSERT INTO conversations (id, kind, name) VALUES (1, 'group', 'g')")
    .execute(pool)
    .await
    .unwrap();

  // An ID created "now" must go into a monthly partition, not the default one.
  let id = SnowflakeGenerator::new(1)
    .unwrap()
    .next_snowflake()
    .unwrap()
    .get();
  sqlx::query(
    "INSERT INTO messages (id, conversation_id, sender_device_id, content_key_id, ciphertext) \
     VALUES ($1, 1, 1, '\\x01', '\\x02')",
  )
  .bind(id)
  .execute(pool)
  .await
  .unwrap();
  let partition: String = sqlx::query_scalar("SELECT tableoid::regclass::text FROM messages WHERE id = $1")
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap();
  assert_ne!(partition, "messages_default");
  assert!(
    partition.starts_with("messages_20"),
    "unexpected partition {partition}"
  );

  // Constraint: exactly one of inline ciphertext and S3 reference.
  let both_missing = sqlx::query(
    "INSERT INTO messages (id, conversation_id, sender_device_id, content_key_id) VALUES ($1 + 1, 1, 1, '\\x01')",
  )
  .bind(id)
  .execute(pool)
  .await;
  assert!(both_missing.is_err());
  test_db.drop_database().await;
}

struct TestDatabase {
  name: String,
  admin_url: String,
  pool: PgPool,
  db: Database,
}

impl TestDatabase {
  /// `None` (test is skipped) when `DATABASE_URL` is not set.
  async fn create() -> Option<Self> {
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
    let pool = PgPool::connect(&format!("{base}/{name}")).await.unwrap();
    let db = Database::from_pool(pool.clone());
    db.migrate().await.unwrap();
    Some(Self {
      name,
      admin_url,
      pool,
      db,
    })
  }

  async fn drop_database(self) {
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

fn identity<'a>(subject: &'a str, display_name: &'a str) -> OidcIdentity<'a> {
  OidcIdentity {
    issuer: ISSUER,
    subject,
    maybe_username: None,
    display_name,
    kind: UserKind::Member,
  }
}
