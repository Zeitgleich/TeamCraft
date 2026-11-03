//! `unread_update` gateway events (issue #183) against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

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

#[tokio::test]
async fn moving_the_read_position_sends_the_new_counts_to_the_readers_devices() {
  let app = app!();
  let (alice, bob) = (owner(), member("bob"));
  let alice_device = register_device(&app, &alice).await;
  let bob_device = register_device(&app, &bob).await;
  let bob_id = user_id(&app, &bob).await;
  let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await.id();
  let first = send(&app, &alice, &dm, &alice_device, 1).await;
  send_in(&app, &alice, &dm, &alice_device, 2, None, &[&bob_id]).await;
  let last = send(&app, &alice, &dm, &alice_device, 3).await;
  let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
  let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
  ws::events(&mut bob_socket, QUIET).await;
  ws::events(&mut alice_socket, QUIET).await;

  mark_read(&app, &bob, &dm, &first).await;
  assert_eq!(
    unread_events(&ws::events(&mut bob_socket, QUIET).await),
    vec![json!({"type": "unread_update", "conversation_id": dm, "unread_count": 2, "mention_count": 1})]
  );

  mark_read(&app, &bob, &dm, &last).await;
  assert_eq!(
    unread_events(&ws::events(&mut bob_socket, QUIET).await),
    vec![json!({"type": "unread_update", "conversation_id": dm, "unread_count": 0, "mention_count": 0})]
  );
  // Only the reader's own devices hear about their counts.
  assert!(unread_events(&ws::events(&mut alice_socket, QUIET).await).is_empty());
  app.stop().await;
}

/// A queued read position that arrives late (a device that was offline, a retry after a lost
/// answer) never moves the position back (issue #159).
#[tokio::test]
async fn a_late_older_read_position_never_moves_the_position_back() {
  let app = app!();
  let (alice, bob) = (owner(), member("bob"));
  let alice_device = register_device(&app, &alice).await;
  let bob_id = user_id(&app, &bob).await;
  let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await.id();
  let first = send(&app, &alice, &dm, &alice_device, 1).await;
  let last = send(&app, &alice, &dm, &alice_device, 2).await;

  mark_read(&app, &bob, &dm, &last).await;
  // The same write again, then an older one: both are accepted and change nothing.
  mark_read(&app, &bob, &dm, &last).await;
  mark_read(&app, &bob, &dm, &first).await;
  let unread = app.get(&bob, "/unread").await;
  assert_eq!(unread.status, StatusCode::OK, "{}", unread.body);
  let counts = unread.body["conversations"]
    .as_array()
    .unwrap()
    .iter()
    .find(|entry| entry["conversation_id"] == dm.as_str())
    .cloned()
    .unwrap_or_default();
  assert_eq!(counts["unread_count"].as_u64().unwrap_or(0), 0, "{}", unread.body);
  let conversation = app.get(&bob, &format!("/conversations/{dm}")).await;
  assert_eq!(
    conversation.body["my_membership"]["last_read_message_id"],
    last.as_str(),
    "{}",
    conversation.body
  );
  app.stop().await;
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

fn unread_events(events: &[GatewayEvent]) -> Vec<Value> {
  events
    .iter()
    .map(|event| serde_json::to_value(event).unwrap())
    .filter(|json| json["type"] == "unread_update")
    .collect()
}
