//! Delivered ticks (issue #598) in DMs and group DMs against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{GatewayClientMessage, GatewayEvent};
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
async fn a_dm_partner_device_moves_the_senders_ticks() {
  let app = app!();
  let (alice, bob) = (owner(), member("bob"));
  let (alice_device, bob_device) = (
    register_device(&app, &alice).await,
    register_device(&app, &bob).await,
  );
  let bob_id = user_id(&app, &bob).await;
  let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await.id();
  let first = send(&app, &alice, &dm, &alice_device, 1).await;
  let second = send(&app, &alice, &dm, &alice_device, 2).await;
  let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
  let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
  ws::events(&mut alice_socket, QUIET).await;

  ws::send_frame(&mut bob_socket, &delivered(&dm, &second)).await;
  let seen = delivery_events(&ws::events(&mut alice_socket, QUIET).await);
  assert_eq!(
    seen,
    vec![json!({"type": "delivery_update", "conversation_id": dm, "message_id": second})]
  );
  assert_eq!(delivered_up_to(&app, &alice, &dm).await, json!(second));
  // Alice's devices never reported, so bob sees nothing.
  assert_eq!(delivered_up_to(&app, &bob, &dm).await, Value::Null);

  // An older report doesn't move it back, and a repeat says nothing again.
  ws::send_frame(&mut bob_socket, &delivered(&dm, &first)).await;
  ws::send_frame(&mut bob_socket, &delivered(&dm, &second)).await;
  assert!(delivery_events(&ws::events(&mut alice_socket, QUIET).await).is_empty());
  assert_eq!(delivered_up_to(&app, &alice, &dm).await, json!(second));
  app.stop().await;
}

#[tokio::test]
async fn delivery_is_reciprocal() {
  let app = app!();
  let (alice, bob) = (owner(), member("bob"));
  let (alice_device, bob_device) = (
    register_device(&app, &alice).await,
    register_device(&app, &bob).await,
  );
  let bob_id = user_id(&app, &bob).await;
  let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await.id();
  let message = send(&app, &alice, &dm, &alice_device, 1).await;
  let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
  let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;

  set_delivery(&app, &bob, false).await;
  ws::send_frame(&mut bob_socket, &delivered(&dm, &message)).await;
  ws::send_frame(&mut alice_socket, &delivered(&dm, &message)).await;
  assert!(delivery_events(&ws::events(&mut alice_socket, QUIET).await).is_empty());
  assert!(delivery_events(&ws::events(&mut bob_socket, QUIET).await).is_empty());
  assert_eq!(
    delivered_up_to(&app, &alice, &dm).await,
    Value::Null,
    "bob doesn't share it"
  );
  assert_eq!(
    delivered_up_to(&app, &bob, &dm).await,
    Value::Null,
    "nor does he see it"
  );

  // The setting reads back; a client that leaves it out keeps it.
  let settings = app.get(&bob, "/me/notification-settings").await.body;
  assert_eq!(settings["send_delivery_receipts"], json!(false));
  let reply = app
    .put(
      &bob,
      "/me/notification-settings",
      json!({"auto_follow_mentioned_threads": true, "push_mode": "away_only", "send_read_receipts": true}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  assert_eq!(reply.body["send_delivery_receipts"], json!(false));
  app.stop().await;
}

#[tokio::test]
async fn a_group_dm_counts_once_every_other_member_has_it() {
  let app = app!();
  let (alice, bob, carol) = (owner(), member("bob"), member("carol"));
  let alice_device = register_device(&app, &alice).await;
  let (bob_device, carol_device) = (
    register_device(&app, &bob).await,
    register_device(&app, &carol).await,
  );
  let (bob_id, carol_id) = (user_id(&app, &bob).await, user_id(&app, &carol).await);
  let group = app
    .post(&alice, "/group-dms", json!({"user_ids": [bob_id, carol_id]}))
    .await
    .id();
  let message = send(&app, &alice, &group, &alice_device, 1).await;
  let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
  let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
  let mut carol_socket = ws::connect(&app, &carol, &carol_device).await;
  ws::events(&mut alice_socket, QUIET).await;

  ws::send_frame(&mut bob_socket, &delivered(&group, &message)).await;
  assert!(delivery_events(&ws::events(&mut alice_socket, QUIET).await).is_empty());
  assert_eq!(delivered_up_to(&app, &alice, &group).await, Value::Null);

  ws::send_frame(&mut carol_socket, &delivered(&group, &message)).await;
  assert_eq!(
    delivery_events(&ws::events(&mut alice_socket, QUIET).await),
    vec![json!({"type": "delivery_update", "conversation_id": group, "message_id": message})]
  );
  assert_eq!(delivered_up_to(&app, &alice, &group).await, json!(message));
  app.stop().await;
}

#[tokio::test]
async fn channels_and_strangers_change_nothing() {
  let app = app!();
  let (alice, bob, mallory) = (owner(), member("bob"), member("mallory"));
  let alice_device = register_device(&app, &alice).await;
  let (bob_device, mallory_device) = (
    register_device(&app, &bob).await,
    register_device(&app, &mallory).await,
  );
  let bob_id = user_id(&app, &bob).await;
  let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await.id();
  let message = send(&app, &alice, &dm, &alice_device, 1).await;
  let channel = channel_with(&app, &[&bob]).await;
  let channel_message = send(&app, &alice, &channel, &alice_device, 2).await;
  let mut alice_socket = ws::connect(&app, &alice, &alice_device).await;
  let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
  let mut mallory_socket = ws::connect(&app, &mallory, &mallory_device).await;
  ws::events(&mut alice_socket, QUIET).await;

  ws::send_frame(&mut bob_socket, &delivered(&channel, &channel_message)).await;
  ws::send_frame(&mut mallory_socket, &delivered(&dm, &message)).await;
  // A message of another conversation can't move the DM's position.
  ws::send_frame(&mut bob_socket, &delivered(&dm, &channel_message)).await;
  assert!(delivery_events(&ws::events(&mut alice_socket, QUIET).await).is_empty());
  assert_eq!(delivered_up_to(&app, &alice, &dm).await, Value::Null);
  assert_eq!(delivered_up_to(&app, &alice, &channel).await, Value::Null);
  app.stop().await;
}

fn delivered(conversation: &str, message: &str) -> GatewayClientMessage {
  serde_json::from_value(json!({"type": "delivered", "conversation_id": conversation, "message_id": message}))
    .unwrap()
}

async fn delivered_up_to(app: &TestApp, user: &TestUser, conversation: &str) -> Value {
  let reply = app
    .get(user, &format!("/conversations/{conversation}/read-receipts"))
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body.get("delivered_up_to").cloned().unwrap_or(Value::Null)
}

async fn set_delivery(app: &TestApp, user: &TestUser, is_enabled: bool) {
  let reply = app
    .put(
      user,
      "/me/notification-settings",
      json!({"auto_follow_mentioned_threads": true, "push_mode": "away_only", "send_read_receipts": true,
        "send_delivery_receipts": is_enabled}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
}

fn delivery_events(events: &[GatewayEvent]) -> Vec<Value> {
  events
    .iter()
    .map(|event| serde_json::to_value(event).unwrap())
    .filter(|json| json["type"] == "delivery_update")
    .collect()
}
