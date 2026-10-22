//! Shared helpers of the integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod fake_keycloak;

use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::{TestApp, TestUser};

pub fn owner() -> TestUser {
  TestUser::new("alice", &["owner"])
}

pub fn admin() -> TestUser {
  TestUser::new("bob", &["admin"])
}

pub fn member(name: &str) -> TestUser {
  TestUser::new(name, &["member"])
}

pub fn guest(name: &str) -> TestUser {
  TestUser::new(name, &["guest"])
}

/// The user's ID (creates the account on first use).
pub async fn user_id(app: &TestApp, user: &TestUser) -> String {
  let reply = app.get(user, "/me").await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.id()
}

pub async fn register_device(app: &TestApp, user: &TestUser) -> String {
  let reply = app
    .post(
      user,
      "/devices",
      json!({"name": "laptop", "public_key": "cHVibGlj", "cross_signature": "c2ln"}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.id()
}

pub async fn create_space(app: &TestApp, user: &TestUser, name: &str, join_policy: &str) -> String {
  let reply = app
    .post(user, "/spaces", json!({"name": name, "join_policy": join_policy}))
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.id()
}

pub async fn create_channel(
  app: &TestApp,
  user: &TestUser,
  space: &str,
  name: &str,
  visibility: &str,
) -> String {
  let reply = app
    .post(
      user,
      &format!("/spaces/{space}/channels"),
      json!({"name": name, "visibility": visibility}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.id()
}

pub async fn add_space_member(app: &TestApp, by: &TestUser, space: &str, user: &str, role: &str) {
  let reply = app
    .post(
      by,
      &format!("/spaces/{space}/members"),
      json!({"user_id": user, "role": role}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

pub async fn add_channel_member(app: &TestApp, by: &TestUser, channel: &str, user: &str) {
  let reply = app
    .post(
      by,
      &format!("/conversations/{channel}/members"),
      json!({"user_id": user}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

/// A distinct 32-hex-character client message ID.
pub fn client_id(n: u8) -> String {
  format!("{n:02x}").repeat(16)
}

pub fn message_body(n: u8, device: &str) -> Value {
  json!({
    "client_message_id": client_id(n),
    "device_id": device,
    "content_key_id": "a2V5",
    "ciphertext": "Y2lwaGVydGV4dA==",
  })
}

/// Sends a message and returns its server ID.
pub async fn send(app: &TestApp, user: &TestUser, conversation: &str, device: &str, n: u8) -> String {
  let reply = app
    .post(
      user,
      &format!("/conversations/{conversation}/messages"),
      message_body(n, device),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.id()
}

/// alice owns a space; carol and dave are members of its private channel (returned).
pub async fn channel_with(app: &TestApp, members: &[&TestUser]) -> String {
  let alice = owner();
  let space = create_space(app, &alice, "Team", "invite_only").await;
  let channel = create_channel(app, &alice, &space, "secure", "private").await;
  for member in members {
    let id = user_id(app, member).await;
    add_space_member(app, &alice, &space, &id, "member").await;
    add_channel_member(app, &alice, &channel, &id).await;
  }
  channel
}
