//! Key bag storage against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn bag(expected: u64, content: &str) -> serde_json::Value {
  json!({"expected_version": expected, "encrypted_key_bag": content, "wrapped_backup_keys": "d3JhcHBlZA=="})
}

#[tokio::test]
async fn a_key_bag_is_created_read_back_and_versioned() {
  let app = app!();
  let carol = member("carol");
  assert_eq!(
    app.get(&carol, "/me/key-bag").await.error_code(),
    "key_bag_not_found"
  );

  let created = app.put(&carol, "/me/key-bag", bag(0, "Ym9uZTE=")).await;
  assert_eq!(created.status, StatusCode::OK, "{}", created.body);
  assert_eq!(created.body["version"], 1);
  let updated = app.put(&carol, "/me/key-bag", bag(1, "Ym9uZTI=")).await;
  assert_eq!(updated.body["version"], 2);

  let read = app.get(&carol, "/me/key-bag").await;
  assert_eq!(read.body["encrypted_key_bag"], "Ym9uZTI=");
  assert_eq!(read.body["wrapped_backup_keys"], "d3JhcHBlZA==");
  // Key bags are private to their owner.
  assert_eq!(
    app.get(&member("dave"), "/me/key-bag").await.status,
    StatusCode::NOT_FOUND
  );
  app.stop().await;
}

#[tokio::test]
async fn stale_versions_are_rejected_without_changing_the_bag() {
  let app = app!();
  let carol = member("carol");
  app.put(&carol, "/me/key-bag", bag(0, "Ym9uZTE=")).await;
  app.put(&carol, "/me/key-bag", bag(1, "Ym9uZTI=")).await;

  let stale = app.put(&carol, "/me/key-bag", bag(1, "c3RhbGU=")).await;
  assert_eq!(
    (stale.status, stale.error_code()),
    (StatusCode::CONFLICT, "version_conflict")
  );
  assert!(stale.body["message"].as_str().unwrap().contains('2'));
  assert_eq!(
    stale.body["current_version"], 2,
    "the version is a structured field"
  );
  assert!(stale.body.get("current_epoch").is_none());
  let again_create = app.put(&carol, "/me/key-bag", bag(0, "c3RhbGU=")).await;
  assert_eq!(again_create.error_code(), "version_conflict");
  let future = app.put(&carol, "/me/key-bag", bag(9, "c3RhbGU=")).await;
  assert_eq!(future.error_code(), "version_conflict");
  assert_eq!(
    app.get(&carol, "/me/key-bag").await.body["encrypted_key_bag"],
    "Ym9uZTI="
  );
  app.stop().await;
}

#[tokio::test]
async fn of_concurrent_writers_with_one_version_exactly_one_wins() {
  let app = app!();
  let carol = member("carol");
  app.put(&carol, "/me/key-bag", bag(0, "Ym9uZTE=")).await;

  let writers = (0..10).map(|n| {
    let (app, carol) = (&app, &carol);
    async move {
      app
        .put(carol, "/me/key-bag", bag(1, &format!("{:0>4}", n + 1000)))
        .await
    }
  });
  let replies = futures_util::future::join_all(writers).await;
  assert_eq!(replies.iter().filter(|r| r.status == StatusCode::OK).count(), 1);
  assert_eq!(
    replies
      .iter()
      .filter(|r| r.error_code_or("") == "version_conflict")
      .count(),
    9
  );
  assert_eq!(app.get(&carol, "/me/key-bag").await.body["version"], 2);
  app.stop().await;
}

#[tokio::test]
async fn empty_and_oversized_bags_are_rejected() {
  let Some(app) = TestApp::start_with(|s| s.limits.max_key_bag_bytes = 16).await else {
    return;
  };
  let carol = member("carol");
  let empty = app.put(&carol, "/me/key-bag", bag(0, "")).await;
  assert_eq!(empty.status, StatusCode::BAD_REQUEST);
  let big = app.put(&carol, "/me/key-bag", bag(0, &"QUJD".repeat(20))).await;
  assert_eq!(
    (big.status, big.error_code()),
    (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large")
  );
  app.stop().await;
}
