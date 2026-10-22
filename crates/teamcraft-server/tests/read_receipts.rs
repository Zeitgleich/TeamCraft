//! Read receipts in DMs and group DMs against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::GatewayEvent;
use teamcraft_server::testing::{TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

const QUIET: Duration = Duration::from_millis(400);

async fn set_receipts(app: &TestApp, user: &TestUser, is_enabled: bool) {
  let reply = app
    .put(
      user,
      "/me/notification-settings",
      json!({"auto_follow_mentioned_threads": true, "push_mode": "away_only", "send_read_receipts": is_enabled}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}

async fn mark_read(app: &TestApp, user: &TestUser, conversation: &str, message: &str) {
  let reply = app
    .put(
      user,
      &format!("/conversations/{conversation}/read-position"),
      json!({"message_id": message}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
}

async fn receipts(app: &TestApp, user: &TestUser, conversation: &str) -> Value {
  let reply = app
    .get(user, &format!("/conversations/{conversation}/read-receipts"))
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["receipts"].clone()
}

fn receipt_events(events: &[GatewayEvent]) -> Vec<Value> {
  events
    .iter()
    .map(|event| serde_json::to_value(event).unwrap())
    .filter(|json| json["type"] == "read_receipt")
    .collect()
}

#[tokio::test]
async fn readers_are_listed_and_announced_to_the_other_participants() {
  let app = app!();
  let (alice, bob) = (owner(), member("bob"));
  let (alice_device, _) = (
    register_device(&app, &alice).await,
    register_device(&app, &bob).await,
  );
  let (alice_id, bob_id) = (user_id(&app, &alice).await, user_id(&app, &bob).await);
  let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await.id();
  let message = send(&app, &alice, &dm, &alice_device, 1).await;
  let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
  ws::events(&mut alice_socket, QUIET).await;

  mark_read(&app, &bob, &dm, &message).await;

  let seen = receipt_events(&ws::events(&mut alice_socket, QUIET).await);
  assert_eq!(seen.len(), 1, "{seen:?}");
  assert_eq!(
    (
      seen[0]["conversation_id"].as_str(),
      seen[0]["user_id"].as_str(),
      seen[0]["message_id"].as_str()
    ),
    (Some(dm.as_str()), Some(bob_id.as_str()), Some(message.as_str()))
  );
  assert_eq!(
    receipts(&app, &alice, &dm).await,
    json!([{"user_id": bob_id, "message_id": message}])
  );
  // Alice has read nothing, so bob sees no receipt of hers.
  assert_eq!(receipts(&app, &bob, &dm).await, json!([]));
  mark_read(&app, &alice, &dm, &message).await;
  assert_eq!(
    receipts(&app, &bob, &dm).await,
    json!([{"user_id": alice_id, "message_id": message}])
  );
  app.stop().await;
}

#[tokio::test]
async fn receipts_are_reciprocal() {
  let app = app!();
  let (alice, bob) = (owner(), member("bob"));
  let alice_device = register_device(&app, &alice).await;
  register_device(&app, &bob).await;
  let bob_id = user_id(&app, &bob).await;
  let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await.id();
  let message = send(&app, &alice, &dm, &alice_device, 1).await;
  let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
  ws::events(&mut alice_socket, QUIET).await;

  set_receipts(&app, &bob, false).await;
  mark_read(&app, &bob, &dm, &message).await;
  mark_read(&app, &alice, &dm, &message).await;

  assert!(receipt_events(&ws::events(&mut alice_socket, QUIET).await).is_empty());
  assert_eq!(
    receipts(&app, &alice, &dm).await,
    json!([]),
    "bob does not send receipts"
  );
  assert_eq!(
    receipts(&app, &bob, &dm).await,
    json!([]),
    "bob does not see receipts either"
  );
  app.stop().await;
}

#[tokio::test]
async fn group_dms_have_receipts_but_channels_do_not() {
  let app = app!();
  let (alice, bob, carol) = (owner(), member("bob"), member("carol"));
  let alice_device = register_device(&app, &alice).await;
  let (bob_id, carol_id) = (user_id(&app, &bob).await, user_id(&app, &carol).await);
  let group = app
    .post(&alice, "/group-dms", json!({"user_ids": [bob_id, carol_id]}))
    .await
    .id();
  let message = send(&app, &alice, &group, &alice_device, 1).await;
  mark_read(&app, &bob, &group, &message).await;
  mark_read(&app, &carol, &group, &message).await;
  assert_eq!(receipts(&app, &alice, &group).await.as_array().unwrap().len(), 2);

  let channel = channel_with(&app, &[&bob]).await;
  let channel_message = send(&app, &alice, &channel, &alice_device, 2).await;
  mark_read(&app, &bob, &channel, &channel_message).await;
  assert_eq!(receipts(&app, &alice, &channel).await, json!([]));
  let stranger = app
    .get(&carol, &format!("/conversations/{channel}/read-receipts"))
    .await;
  assert_eq!(stranger.status, StatusCode::NOT_FOUND);
  app.stop().await;
}
