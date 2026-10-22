//! Load test: one channel with millions of message rows (ciphertext), as ARCHITECTURE.md §1.1
//! promises to handle. Measures history paging, unread counts, sends and retention, and checks
//! that the queries use the indexes. Slow, so it only runs on request:
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p teamcraft-server --features integration \
//!   --test perf_history -- --ignored --nocapture
//! ```
//!
//! `TEAMCRAFT_PERF_ROWS` (default 10000000) sets the size. Needs about 1 GB of disk per 2.5M rows.
//! The numbers in docs/performance.md come from this test.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use crate::common;

use std::time::{Duration, Instant};

use common::*;
use serde_json::json;
use teamcraft_db::testing::{PgPool, sqlx};
use teamcraft_proto::SNOWFLAKE_EPOCH_MS;
use teamcraft_server::services::retention::run_once;
use teamcraft_server::testing::TestApp;
use time::OffsetDateTime;

const DEFAULT_ROWS: i64 = 10_000_000;
/// IDs count from 2026-01-01 (SNOWFLAKE_EPOCH_MS), so the data cannot start earlier: about nine
/// months of history, which is also where retention cuts below.
const MONTHS: i64 = 9;
const MS_PER_MONTH: i64 = 30 * 24 * 3600 * 1000;
const SEED_BATCH: i64 = 500_000;

fn percentile(sorted: &[Duration], p: f64) -> Duration {
  let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
  sorted[index]
}

async fn measure<F, Fut>(label: &str, runs: usize, mut f: F) -> Duration
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = ()>,
{
  let mut times = Vec::with_capacity(runs);
  for _ in 0..runs {
    let start = Instant::now();
    f().await;
    times.push(start.elapsed());
  }
  times.sort();
  let (p50, p95, p99) = (
    percentile(&times, 0.5),
    percentile(&times, 0.95),
    percentile(&times, 0.99),
  );
  println!(
    "PERF {label}: p50 {p50:?} p95 {p95:?} p99 {p99:?} max {:?} ({runs} runs)",
    times[times.len() - 1]
  );
  p50
}

/// Runs EXPLAIN ANALYZE and returns (execution time in ms, the plan text).
async fn explain(pool: &PgPool, sql: &str) -> (f64, String) {
  let rows: Vec<String> =
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("EXPLAIN (ANALYZE, BUFFERS) {sql}")))
      .fetch_all(pool)
      .await
      .unwrap();
  let plan = rows.join("\n");
  let millis = plan
    .lines()
    .find_map(|line| line.trim().strip_prefix("Execution Time: "))
    .and_then(|rest| rest.trim_end_matches(" ms").parse().ok())
    .unwrap_or(f64::NAN);
  (millis, plan)
}

#[tokio::test]
#[ignore = "load test, minutes of runtime and gigabytes of disk; run with --ignored"]
async fn history_counts_and_retention_on_millions_of_rows() {
  let _ = tracing_subscriber::fmt()
    .with_max_level(tracing::Level::WARN)
    .try_init();
  let rows: i64 = std::env::var("TEAMCRAFT_PERF_ROWS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(DEFAULT_ROWS);
  let Some(app) = TestApp::start().await else { return };
  let pool = app.sql_pool().clone();
  let (alice, carol) = (owner(), member("carol"));
  let channel = channel_with(&app, &[&carol]).await;
  let alice_device = register_device(&app, &alice).await;
  let alice_id = user_id(&app, &alice).await;
  let carol_id = user_id(&app, &carol).await;

  // Seed: `rows` messages spread evenly over MONTHS months, newest about now.
  let span_ms = MONTHS * MS_PER_MONTH;
  let start_ms =
    i64::try_from(OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000).unwrap() - span_ms;
  let step_ms = (span_ms / rows).max(1);
  sqlx::query("SELECT tc_ensure_month_partitions('messages', timestamptz '2026-01-01 00:00:00+00', $1)")
    .bind(i32::try_from(MONTHS).unwrap() + 3)
    .execute(&pool)
    .await
    .unwrap();
  let seed_started = Instant::now();
  let mut from = 0;
  while from < rows {
    let to = (from + SEED_BATCH).min(rows) - 1;
    sqlx::query(
      r"INSERT INTO messages (id, conversation_id, sender_device_id, sender_user_id, content_key_id,
                              mentioned_user_ids, ciphertext, client_message_id, is_urgent)
        SELECT ((($1 + g * $2) - $3) << 22) | (g % 4096), $4, $5, $6, '\x6b6579'::bytea, '{}'::bigint[],
               decode(repeat('ab', 200), 'hex'), decode(lpad(to_hex(g), 32, '0'), 'hex'), false
        FROM generate_series($7::bigint, $8::bigint) AS g",
    )
    .bind(start_ms)
    .bind(step_ms)
    .bind(SNOWFLAKE_EPOCH_MS)
    .bind(channel.parse::<i64>().unwrap())
    .bind(alice_device.parse::<i64>().unwrap())
    .bind(alice_id.parse::<i64>().unwrap())
    .bind(from)
    .bind(to)
    .execute(&pool)
    .await
    .unwrap();
    from = to + 1;
  }
  // Autovacuum would do this soon after a bulk load; without it index scans pay heap visibility checks.
  sqlx::query("VACUUM (ANALYZE) messages")
    .execute(&pool)
    .await
    .unwrap();
  let size: i64 = sqlx::query_scalar("SELECT sum(pg_total_relation_size(inhrelid))::bigint FROM pg_inherits WHERE inhparent = 'messages'::regclass")
    .fetch_one(&pool)
    .await
    .unwrap();
  println!(
    "PERF seeded {rows} rows in {:?}, {} MiB with indexes",
    seed_started.elapsed(),
    size / (1024 * 1024)
  );

  // alice has been in the channel since the start of the data; carol (from_join) joined just now.
  sqlx::query("UPDATE memberships SET joined_at = timestamptz '2026-01-02 00:00:00+00' WHERE user_id = $1")
    .bind(alice_id.parse::<i64>().unwrap())
    .execute(&pool)
    .await
    .unwrap();

  // History paging through the API: newest page, a deep cursor, forward paging.
  let newest = app
    .get(&alice, &format!("/conversations/{channel}/messages?limit=50"))
    .await;
  assert_eq!(newest.status, reqwest::StatusCode::OK, "{}", newest.body);
  assert_eq!(
    newest.body["messages"].as_array().unwrap().len(),
    50,
    "{}",
    newest.body
  );
  let middle_id = {
    let mut id: String = newest.body["messages"][49]["id"].as_str().unwrap().to_owned();
    // Walk back ~100 pages to get a cursor deep in history without millions of requests.
    for _ in 0..100 {
      let page = app
        .get(
          &alice,
          &format!("/conversations/{channel}/messages?limit=50&before={id}"),
        )
        .await;
      id = page.body["messages"][49]["id"].as_str().unwrap().to_owned();
    }
    id
  };
  let latest = measure("history: newest page of 50 (API, alice)", 200, || async {
    app
      .get(&alice, &format!("/conversations/{channel}/messages?limit=50"))
      .await;
  })
  .await;
  measure("history: page 100 pages back (API)", 200, || async {
    app
      .get(
        &alice,
        &format!("/conversations/{channel}/messages?limit=50&before={middle_id}"),
      )
      .await;
  })
  .await;
  measure("history: forward page after a deep cursor (API)", 100, || async {
    app
      .get(
        &alice,
        &format!("/conversations/{channel}/messages?limit=50&after={middle_id}"),
      )
      .await;
  })
  .await;
  // Carol joined just now (history policy from_join): her view must not scan the old rows.
  let carol_page = measure(
    "history: newest page for a member who just joined (API)",
    100,
    || async {
      app
        .get(&carol, &format!("/conversations/{channel}/messages?limit=50"))
        .await;
    },
  )
  .await;
  assert!(latest < Duration::from_millis(250), "newest page took {latest:?}");
  assert!(
    carol_page < Duration::from_millis(250),
    "from_join page took {carol_page:?}"
  );

  // The query plans.
  let channel_id = channel.clone();
  let latest_sql = format!(
    "SELECT id, ciphertext FROM messages WHERE conversation_id = {channel_id} ORDER BY id DESC LIMIT 51"
  );
  let (millis, plan) = explain(&pool, &latest_sql).await;
  println!("PERF plan newest page: {millis} ms\n{plan}");
  assert!(
    plan.contains("Index") && !plan.contains("Seq Scan"),
    "newest page must use an index:\n{plan}"
  );
  assert!(millis < 50.0, "{millis} ms");
  let deep_sql = format!(
    "SELECT id, ciphertext FROM messages WHERE conversation_id = {channel_id} AND id < {middle_id} ORDER BY id DESC LIMIT 51"
  );
  let (millis, plan) = explain(&pool, &deep_sql).await;
  println!("PERF plan deep page: {millis} ms");
  assert!(
    plan.contains("Index") && !plan.contains("Seq Scan"),
    "deep page must use an index:\n{plan}"
  );

  // Unread counts: a member who has been in the channel since the start and never read anything is
  // the worst case, since every message counts.
  sqlx::query("UPDATE memberships SET joined_at = timestamptz '2026-01-02 00:00:00+00' WHERE user_id = $1")
    .bind(carol_id.parse::<i64>().unwrap())
    .execute(&pool)
    .await
    .unwrap();
  let worst = Instant::now();
  let unread = app.get(&carol, "/unread").await;
  let worst = worst.elapsed();
  println!("PERF unread counts, never read, {rows} messages (API): {worst:?}");
  assert_eq!(
    unread.body["conversations"][0]["unread_count"], 1000,
    "capped at limits.max_unread_counted"
  );
  let unread_sql = format!(
    r"SELECT ms.conversation_id, counted.unread FROM memberships ms CROSS JOIN LATERAL (
        SELECT COUNT(*) AS unread FROM (
          SELECT m.mentioned_user_ids FROM messages m
          WHERE m.conversation_id = ms.conversation_id AND m.thread_id IS NULL AND m.sender_user_id <> ms.user_id
            AND m.id > COALESCE(ms.last_read_message_id, 0) AND m.id >= tc_snowflake_floor(ms.joined_at)
          ORDER BY m.id DESC LIMIT 1000) newest) counted
      WHERE ms.user_id = {carol_id} AND counted.unread > 0"
  );
  let (millis, plan) = explain(&pool, &unread_sql).await;
  println!("PERF plan unread counts, never read: {millis} ms\n{plan}");
  assert!(worst < Duration::from_secs(1), "{worst:?}");
  let near_end = app
    .get(&alice, &format!("/conversations/{channel}/messages?limit=1"))
    .await
    .body["messages"][0]["id"]
    .as_str()
    .unwrap()
    .to_owned();
  app
    .put(
      &carol,
      &format!("/conversations/{channel}/read-position"),
      json!({"message_id": near_end}),
    )
    .await;
  let unread_p50 = measure(
    "unread counts, read position at the newest message (API)",
    50,
    || async {
      app.get(&carol, "/unread").await;
    },
  )
  .await;
  assert!(unread_p50 < Duration::from_millis(100), "{unread_p50:?}");

  // Sends on a big table.
  let device = register_device(&app, &carol).await;
  let mut n = 0_u32;
  measure("send one message (API)", 200, || {
    n += 1;
    let body = {
      let mut body = message_body(1, &device);
      body["client_message_id"] = json!(format!("{n:032x}"));
      body
    };
    let (app, carol, channel) = (&app, &carol, &channel);
    async move {
      let reply = app
        .post(carol, &format!("/conversations/{channel}/messages"), body)
        .await;
      assert_eq!(reply.status, reqwest::StatusCode::CREATED, "{}", reply.body);
    }
  })
  .await;

  // Retention: keep the newer half of the data. Partitions that fall entirely out are the cheap case.
  let partitions_before: i64 =
    sqlx::query_scalar("SELECT count(*) FROM pg_inherits WHERE inhparent = 'messages'::regclass")
      .fetch_one(&pool)
      .await
      .unwrap();
  let set = app
    .put(
      &alice,
      &format!("/conversations/{channel}/retention"),
      json!({"retention": {"max_age_days": MONTHS * 30 / 2}}),
    )
    .await;
  assert_eq!(set.status, reqwest::StatusCode::OK, "{}", set.body);
  let retention_started = Instant::now();
  let report = run_once(&app.state, OffsetDateTime::now_utc()).await.unwrap();
  let partitions_after: i64 =
    sqlx::query_scalar("SELECT count(*) FROM pg_inherits WHERE inhparent = 'messages'::regclass")
      .fetch_one(&pool)
      .await
      .unwrap();
  println!(
    "PERF retention (keep the newer half): {:?}, {} messages deleted, partitions {partitions_before} -> {partitions_after} ({} dropped)",
    retention_started.elapsed(),
    report.messages_deleted,
    report.partitions_dropped
  );
  let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM messages")
    .fetch_one(&pool)
    .await
    .unwrap();
  println!("PERF rows left after retention: {remaining}");
  assert!(
    remaining < rows * 6 / 10 && remaining > rows * 4 / 10,
    "{remaining} rows left"
  );
  let after = measure("history: newest page after retention (API)", 50, || async {
    app
      .get(&alice, &format!("/conversations/{channel}/messages?limit=50"))
      .await;
  })
  .await;
  assert!(after < Duration::from_millis(250));
  app.stop().await;
}
