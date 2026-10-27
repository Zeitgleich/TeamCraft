//! Each channel's signed role log (issue #380) against a real server and Postgres: who may append,
//! ordering, what the payload must name, and who may read it.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::{Reply, TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn channel_admins_append_in_order_and_members_read_the_log() {
  let app = app!();
  let (alice, bob, dave) = (owner(), member("bob"), member("dave"));
  let channel = channel_with(&app, &[&bob]).await;
  let (alice_device, bob_device) = (
    register_device(&app, &alice).await,
    register_device(&app, &bob).await,
  );

  // A member without `manage_channel_roles` appends nothing.
  let denied = append(&app, &bob, &bob_device, &channel, 0, &document(&channel, 1)).await;
  assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);

  let first = append(&app, &alice, &alice_device, &channel, 0, &document(&channel, 1)).await;
  assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
  assert_eq!(first.body["version"], 1);
  assert_eq!(first.body["signer_device_id"], alice_device);

  // A stale version is a conflict that names the current one.
  let stale = append(&app, &alice, &alice_device, &channel, 0, &document(&channel, 1)).await;
  assert_eq!(stale.status, StatusCode::CONFLICT, "{}", stale.body);
  assert_eq!(stale.error_code(), "version_conflict");
  let second = append(&app, &alice, &alice_device, &channel, 1, &document(&channel, 2)).await;
  assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);

  let listed = app.get(&bob, &format!("/conversations/{channel}/role-log")).await;
  assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
  let versions: Vec<Value> = listed.body["entries"]
    .as_array()
    .unwrap()
    .iter()
    .map(|entry| entry["version"].clone())
    .collect();
  assert_eq!(versions, vec![json!(1), json!(2)]);
  let tail = app
    .get(&bob, &format!("/conversations/{channel}/role-log?after=1"))
    .await;
  assert_eq!(tail.body["entries"].as_array().unwrap().len(), 1);

  // Somebody outside the channel reads nothing.
  let outside = app
    .get(&dave, &format!("/conversations/{channel}/role-log"))
    .await;
  assert_ne!(outside.status, StatusCode::OK, "{}", outside.body);
  app.stop().await;
}

#[tokio::test]
async fn a_payload_must_name_this_channel_and_the_next_version() {
  let app = app!();
  let alice = owner();
  let space = create_space(&app, &alice, "Team", "invite_only").await;
  let channel = create_channel(&app, &alice, &space, "first", "private").await;
  let other = create_channel(&app, &alice, &space, "second", "private").await;
  let device = register_device(&app, &alice).await;

  for payload in [
    document(&other, 1),
    document(&channel, 2),
    b"not a document".to_vec(),
  ] {
    let refused = append(&app, &alice, &device, &channel, 0, &payload).await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
  }
  // Somebody else's device can't carry the signature.
  let bob = member("bob");
  let bobs_device = register_device(&app, &bob).await;
  let foreign = append(&app, &alice, &bobs_device, &channel, 0, &document(&channel, 1)).await;
  assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{}", foreign.body);
  app.stop().await;
}

async fn append(
  app: &TestApp,
  user: &TestUser,
  device: &str,
  channel: &str,
  expected: u64,
  payload: &[u8],
) -> Reply {
  app
    .post(
      user,
      &format!("/conversations/{channel}/role-log"),
      json!({"expected_version": expected, "payload": b64(payload), "signature": b64(b"sig"), "signer_device_id": device}),
    )
    .await
}

/// A role snapshot as clients sign it; the server only reads the channel and the version.
fn document(channel: &str, version: u64) -> Vec<u8> {
  serde_json::to_vec(&json!({
    "version": version,
    "conversation_id": channel,
    "assignments": [{"user_id": "1", "role": "admin", "identity_key": b64(&[0; 32])}],
    "prev_hash": b64(&[0; 32]),
    "signed_at_ms": 0,
  }))
  .unwrap()
}

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}
