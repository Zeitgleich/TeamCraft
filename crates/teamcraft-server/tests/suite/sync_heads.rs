//! `GET /sync/heads` (issue #714): the newest message and MLS entry per conversation and the member
//! list digests, against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::{Duration, Instant};

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_db::testing::sqlx;
use teamcraft_server::testing::{TestApp, TestUser};

/// The large workspace of the load test: Carol's channels, the people in the space, members per
/// channel.
const LOAD_CHANNELS: i64 = 300;
const LOAD_PEOPLE: i64 = 2_000;
const LOAD_MEMBERS_PER_CHANNEL: i64 = 200;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn heads_follow_messages_mls_entries_and_members() {
  let app = app!();
  let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let carol_device = register_device(&app, &carol).await;
  register_device(&app, &dave).await;

  let empty = head_of(&app, &carol, &channel).await;
  assert_eq!(empty.get("last_message_id"), None);
  assert_eq!(empty.get("last_mls_message_id"), None);
  // The same list for everyone in it: the same digest.
  assert_eq!(
    head_of(&app, &dave, &channel).await["members_digest"],
    empty["members_digest"]
  );

  let message = send(&app, &carol, &channel, &carol_device, 1).await;
  let reply = app
    .post(
      &carol,
      &format!("/conversations/{channel}/mls/commits"),
      json!({"device_id": carol_device, "epoch": 0, "commit": "Y29tbWl0", "welcomes": []}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  let backlog = app.get(&carol, &format!("/conversations/{channel}/mls")).await;
  let newest_mls = backlog.body["messages"].as_array().unwrap().last().unwrap()["id"].clone();
  let after = head_of(&app, &carol, &channel).await;
  assert_eq!(after["last_message_id"], json!(message));
  assert_eq!(after["last_mls_message_id"], newest_mls);
  assert_eq!(after["members_digest"], empty["members_digest"]);

  let erin_id = user_id(&app, &erin).await;
  let space = space_of(&app, &channel).await;
  add_space_member(&app, &owner(), &space, &erin_id, "member").await;
  add_channel_member(&app, &owner(), &channel, &erin_id).await;
  assert_ne!(
    head_of(&app, &carol, &channel).await["members_digest"],
    empty["members_digest"]
  );
  app.stop().await;
}

#[tokio::test]
async fn only_the_callers_conversations_and_no_space_lists_for_guests() {
  let app = app!();
  let (carol, bob, gina) = (member("carol"), member("bob"), guest("gina"));
  let channel = channel_with(&app, &[&carol]).await;
  let space = space_of(&app, &channel).await;
  let gina_id = user_id(&app, &gina).await;
  add_space_member(&app, &owner(), &space, &gina_id, "guest").await;
  add_channel_member(&app, &owner(), &channel, &gina_id).await;

  let outsider = heads(&app, &bob).await;
  assert_eq!(outsider["conversations"], json!([]));
  assert_eq!(outsider["spaces"], json!([]));

  let member_view = heads(&app, &carol).await;
  assert_eq!(member_view["spaces"][0]["space_id"], json!(space));
  let before = member_view["spaces"][0]["members_digest"].clone();

  let guest_view = heads(&app, &gina).await;
  assert_eq!(guest_view["conversations"][0]["conversation_id"], json!(channel));
  assert_eq!(guest_view["spaces"], json!([]));

  let bob_id = user_id(&app, &bob).await;
  add_space_member(&app, &owner(), &space, &bob_id, "member").await;
  assert_ne!(heads(&app, &carol).await["spaces"][0]["members_digest"], before);
  app.stop().await;
}

/// How long the server takes for the heads of a large workspace (issue #714): 300 channels of 200
/// members each in a space of 2,000 people, seeded in SQL. The numbers are in
/// docs/performance.md ("Coming back to 300 channels").
///
/// ```sh
/// cargo test -p teamcraft-server --features integration --test suite -- --ignored --nocapture \
///   sync_heads::heads_of_a_large_workspace
/// ```
#[tokio::test]
#[ignore = "load test; run with --ignored --nocapture"]
#[allow(clippy::print_stdout)]
async fn heads_of_a_large_workspace() {
  let app = app!();
  let carol = member("carol");
  let channel = channel_with(&app, &[&carol]).await;
  let space = space_of(&app, &channel).await;
  let carol_id = user_id(&app, &carol).await;
  seed_large_workspace(&app, &channel, &space, &carol_id).await;

  let mut times = Vec::new();
  let mut maybe_bytes = None;
  for _ in 0..20 {
    let started = Instant::now();
    let reply = app.get(&carol, "/sync/heads").await;
    times.push(started.elapsed());
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(
      reply.body["conversations"].as_array().unwrap().len(),
      LOAD_CHANNELS as usize
    );
    maybe_bytes = Some(reply.body.to_string().len());
  }
  times.sort();
  println!(
    "PERF sync heads, {LOAD_CHANNELS} channels x {LOAD_MEMBERS_PER_CHANNEL} members, space of {LOAD_PEOPLE}: \
     p50 {:?} max {:?}, {} bytes",
    times[times.len() / 2],
    times[times.len() - 1],
    maybe_bytes.unwrap_or_default()
  );
  assert!(times[times.len() / 2] < Duration::from_secs(2));
  app.stop().await;
}

async fn seed_large_workspace(app: &TestApp, channel: &str, space: &str, carol: &str) {
  let pool = app.sql_pool();
  let channel: i64 = channel.parse().unwrap();
  let space: i64 = space.parse().unwrap();
  let carol: i64 = carol.parse().unwrap();
  // IDs far above what the server generates in a test.
  let people = 9_000_000_000_000_000_i64;
  let channels = 9_100_000_000_000_000_i64;
  for sql in [
    "INSERT INTO users (id, username, display_name, kind)
       SELECT $1 + g, 'load' || g, 'Load Person ' || g, 'member' FROM generate_series(1, $2) AS g",
    "INSERT INTO space_members (space_id, user_id, role)
       SELECT $3, $1 + g, 'member' FROM generate_series(1, $2) AS g",
  ] {
    sqlx::query(sqlx::AssertSqlSafe(sql))
      .bind(people)
      .bind(LOAD_PEOPLE)
      .bind(space)
      .execute(pool)
      .await
      .unwrap();
  }
  sqlx::query(
    "INSERT INTO conversations (id, space_id, kind, name, visibility, history_policy)
       SELECT $1 + g, space_id, 'channel', 'load-' || g, visibility, history_policy
       FROM conversations, generate_series(1, $2) AS g WHERE id = $3",
  )
  .bind(channels)
  .bind(LOAD_CHANNELS - 1)
  .bind(channel)
  .execute(pool)
  .await
  .unwrap();
  sqlx::query(
    "INSERT INTO memberships (conversation_id, user_id)
       SELECT $1 + g, $2 FROM generate_series(1, $3) AS g",
  )
  .bind(channels)
  .bind(carol)
  .bind(LOAD_CHANNELS - 1)
  .execute(pool)
  .await
  .unwrap();
  // Each channel (the first one too) gets a window of the space's people.
  sqlx::query(
    "INSERT INTO memberships (conversation_id, user_id)
       SELECT c.id, $1 + 1 + ((c.id + p) % $2)
       FROM conversations c, generate_series(0, $3 - 1) AS p
       WHERE c.space_id = $4
       ON CONFLICT DO NOTHING",
  )
  .bind(people)
  .bind(LOAD_PEOPLE)
  .bind(LOAD_MEMBERS_PER_CHANNEL)
  .bind(space)
  .execute(pool)
  .await
  .unwrap();
}

async fn heads(app: &TestApp, user: &TestUser) -> Value {
  let reply = app.get(user, "/sync/heads").await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body
}

async fn head_of(app: &TestApp, user: &TestUser, conversation: &str) -> Value {
  heads(app, user).await["conversations"]
    .as_array()
    .unwrap()
    .iter()
    .find(|head| head["conversation_id"] == json!(conversation))
    .cloned()
    .unwrap_or_else(|| panic!("no head for {conversation}"))
}

async fn space_of(app: &TestApp, conversation: &str) -> String {
  let reply = app.get(&owner(), &format!("/conversations/{conversation}")).await;
  reply.body["space_id"].as_str().unwrap().to_owned()
}
