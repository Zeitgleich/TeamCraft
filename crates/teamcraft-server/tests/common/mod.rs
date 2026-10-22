//! Shared helpers of the integration tests.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod fake_gateway;
pub mod fake_keycloak;
pub mod push_fakes;

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

/// Sends a message with optional thread and mentions and returns its server ID.
pub async fn send_in(
  app: &TestApp,
  user: &TestUser,
  conversation: &str,
  device: &str,
  n: u8,
  maybe_thread: Option<&str>,
  mentioned: &[&str],
) -> String {
  let mut body = message_body(n, device);
  body["thread_id"] = json!(maybe_thread);
  body["mentioned_user_ids"] = json!(mentioned);
  let reply = app
    .post(user, &format!("/conversations/{conversation}/messages"), body)
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.id()
}

/// A request with an arbitrary bearer token (guest sessions, bot tokens); returns status and JSON.
pub async fn call_with_token(
  app: &TestApp,
  token: &str,
  method: reqwest::Method,
  path: &str,
  maybe_body: Option<Value>,
) -> (StatusCode, Value) {
  let mut request = app
    .http
    .request(method, format!("{}/api/v1{path}", app.base_url))
    .bearer_auth(token);
  if let Some(body) = maybe_body {
    request = request.json(&body);
  }
  let response = request.send().await.unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
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

pub mod ws {
  //! Minimal gateway client for tests that only watch events.
  #![allow(clippy::unwrap_used)]

  use std::time::Duration;

  use futures_util::{SinkExt, StreamExt};
  use serde_json::json;
  use teamcraft_proto::{DeviceActivity, GatewayClientMessage, GatewayEvent, GatewayServerMessage};
  use teamcraft_server::testing::{TestApp, TestUser};
  use tokio_tungstenite::tungstenite::Message;

  pub type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

  /// Connects and identifies; returns once the session is ready.
  pub async fn connect(app: &TestApp, user: &TestUser, device: &str) -> Socket {
    connect_with_token(app, app.token(user), device).await
  }

  /// Like [`connect`] with a bearer token that is not an identity provider token (guest sessions).
  pub async fn connect_with_token(app: &TestApp, token: String, device: &str) -> Socket {
    let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
    socket.next().await;
    let identify = GatewayClientMessage::Identify {
      token,
      device_id: serde_json::from_value(json!(device)).unwrap(),
      activity: DeviceActivity::Active,
    };
    socket
      .send(Message::Text(serde_json::to_string(&identify).unwrap().into()))
      .await
      .unwrap();
    socket.next().await;
    socket
  }

  /// Every event that arrives within `wait`, or until the connection closes.
  pub async fn events(socket: &mut Socket, wait: Duration) -> Vec<GatewayEvent> {
    let mut seen = Vec::new();
    while let Ok(Some(Ok(message))) = tokio::time::timeout(wait, socket.next()).await {
      if let Message::Text(text) = message
        && let Ok(GatewayServerMessage::Event { event, .. }) = serde_json::from_str(text.as_str())
      {
        seen.push(event);
      }
    }
    seen
  }
}
