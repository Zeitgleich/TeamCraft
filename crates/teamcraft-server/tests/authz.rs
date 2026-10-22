//! Authorization of every route, driven by the OpenAPI document so that a new route can't be added
//! without being covered:
//!
//! - every operation that declares bearer security rejects requests without a token and with
//!   garbage tokens (401); operations without security are on an explicit allow list;
//! - `/admin/...` rejects ordinary members and guests (403);
//! - operations on a conversation, space, meeting or call reject people who are not part of it,
//!   members and guests alike, with 4xx (never success, never a server error).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use common::*;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_server::config::{MailTexts, MeetingsConfig};
use teamcraft_server::testing::{FakeCalendar, FakeMailer, TestApp, TestUser};

/// Operations that need no bearer token, and why.
const PUBLIC: &[(&str, &str)] = &[
  ("get", "/api/v1/openapi.json"),
  // Public presentation of the host (name, colors, logo): the login screen needs it before sign-in.
  ("get", "/api/v1/brand"),
  ("get", "/api/v1/brand/theme.css"),
  ("get", "/api/v1/brand/assets/{slot}"),
  // The link's token is the credential.
  ("post", "/api/v1/meetings/join"),
  // Local accounts sign in here: the one-time activation token or the password and the TOTP code
  // are the credentials; failures are counted per account and per address.
  ("post", "/api/v1/auth/local/activate"),
  ("post", "/api/v1/auth/local/login"),
  // The link's ID is the credential (256 random bits); both answer 410 for anything not usable.
  ("get", "/api/v1/shares/{share_id}"),
  ("post", "/api/v1/shares/{share_id}/download"),
  // Authenticated by the logout token's signature.
  ("post", "/api/v1/auth/backchannel-logout"),
  // Authenticated by the SFU's / inbox handler's own bearer token (checked below).
  ("post", "/api/internal/v1/calls/participants/joined"),
  ("post", "/api/internal/v1/calls/participants/left"),
  ("post", "/api/internal/v1/meetings/replies"),
];

/// Operations that legitimately answer other members: the directory, device and key lookups other
/// clients need to add someone to a group.
const OPEN_TO_MEMBERS: &[&str] = &[
  "/users/{user_id}",
  "/users/{user_id}/devices",
  "/users/{user_id}/identity-key",
];

struct Scene {
  meeting: String,
  channel: String,
  space: String,
  alice_id: String,
  alice_device: String,
  eve: TestUser,
  gina: TestUser,
}

/// A server with meetings configured, so meeting routes are really exercised.
async fn start() -> Option<TestApp> {
  let config = MeetingsConfig {
    public_base_url: "https://chat.example.com".into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 10,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    maybe_inbox_token: Some("inbox-token-for-tests".to_owned().into()),
    smtp: None,
    mail_texts: MailTexts::default(),
  };
  TestApp::start_with_meetings(
    &config,
    Arc::new(FakeCalendar::default()),
    Arc::new(FakeMailer::default()),
  )
  .await
}

async fn scene(app: &TestApp) -> Scene {
  let (alice, carol) = (owner(), member("carol"));
  let alice_device = register_device(app, &alice).await;
  let alice_id = user_id(app, &alice).await;
  let space = create_space(app, &alice, "Team", "invite_only").await;
  let channel = create_channel(app, &alice, &space, "secure", "private").await;
  let carol_id = user_id(app, &carol).await;
  add_space_member(app, &alice, &space, &carol_id, "member").await;
  add_channel_member(app, &alice, &channel, &carol_id).await;
  let (eve, gina) = (member("eve"), guest("gina"));
  user_id(app, &eve).await;
  user_id(app, &gina).await;
  let meeting = app
    .post(
      &alice,
      "/meetings",
      json!({"title": "Sync", "starts_at": 1_893_488_400_000_i64, "ends_at": 1_893_492_000_000_i64}),
    )
    .await
    .id();
  Scene {
    meeting,
    channel,
    space,
    alice_id,
    alice_device,
    eve,
    gina,
  }
}

/// (method, openapi path, operation) of every documented operation.
fn operations(document: &Value) -> Vec<(Method, String, Value)> {
  let mut operations = Vec::new();
  for (path, item) in document["paths"].as_object().unwrap() {
    for (verb, operation) in item.as_object().unwrap() {
      let method = Method::from_bytes(verb.to_uppercase().as_bytes()).unwrap();
      operations.push((method, path.clone(), operation.clone()));
    }
  }
  assert!(
    operations.len() > 100,
    "the OpenAPI document looks incomplete: {} operations",
    operations.len()
  );
  operations
}

fn fill(path: &str, scene: &Scene) -> String {
  let values: BTreeMap<&str, &str> = BTreeMap::from([
    ("conversation_id", scene.channel.as_str()),
    ("space_id", scene.space.as_str()),
    ("user_id", scene.alice_id.as_str()),
    ("device_id", scene.alice_device.as_str()),
    ("meeting_id", scene.meeting.as_str()),
  ]);
  let mut filled = path.to_owned();
  while let Some(start) = filled.find('{') {
    let end = filled[start..].find('}').unwrap() + start;
    let name = filled[start + 1..end].to_owned();
    let value = values.get(name.as_str()).copied().unwrap_or("1");
    filled.replace_range(start..=end, value);
  }
  filled
}

async fn send(app: &TestApp, method: &Method, path: &str, maybe_token: Option<&str>) -> (StatusCode, Value) {
  let mut request = app
    .http
    .request(method.clone(), format!("{}{path}", app.base_url));
  if let Some(token) = maybe_token {
    request = request.bearer_auth(token);
  }
  if matches!(*method, Method::POST | Method::PUT | Method::PATCH) {
    request = request.json(&json!({}));
  }
  let response = request.send().await.unwrap();
  let status = response.status();
  (status, response.json().await.unwrap_or(Value::Null))
}

fn is_secured(operation: &Value) -> bool {
  operation["security"]
    .as_array()
    .is_some_and(|security| !security.is_empty())
}

#[tokio::test]
async fn every_operation_needs_a_valid_token_unless_it_is_on_the_public_list() {
  let Some(app) = start().await else { return };
  let scene = scene(&app).await;
  let document = app
    .http
    .get(format!("{}/api/v1/openapi.json", app.base_url))
    .send()
    .await
    .unwrap()
    .json::<Value>()
    .await
    .unwrap();

  let mut checked = 0;
  for (method, path, operation) in operations(&document) {
    let url = fill(&path, &scene);
    let verb = method.as_str().to_lowercase();
    if is_secured(&operation) {
      for token in [None, Some("not-a-token"), Some("tcb_not_a_bot"), Some("a.b.c")] {
        let (status, _) = send(&app, &method, &url, token).await;
        assert_eq!(
          status,
          StatusCode::UNAUTHORIZED,
          "{method} {path} with token {token:?}"
        );
      }
      checked += 1;
    } else {
      assert!(
        PUBLIC.contains(&(verb.as_str(), path.as_str())),
        "{method} {path} has no security and is not on the public list: add security or justify it"
      );
    }
  }
  assert!(checked > 100);

  // Internal endpoints answer without their bearer token (or with a wrong one) with 401, or 503
  // when the feature is not configured; never with success.
  for (method, path) in [
    (Method::POST, "/api/internal/v1/calls/participants/joined"),
    (Method::POST, "/api/internal/v1/meetings/replies"),
  ] {
    for token in [None, Some("wrong")] {
      let (status, _) = send(&app, &method, path, token).await;
      assert!(
        matches!(status, StatusCode::UNAUTHORIZED | StatusCode::SERVICE_UNAVAILABLE),
        "{method} {path}: {status}"
      );
    }
  }
  app.stop().await;
}

#[tokio::test]
async fn admin_operations_reject_members_and_guests() {
  let Some(app) = start().await else { return };
  let scene = scene(&app).await;
  let document = app
    .http
    .get(format!("{}/api/v1/openapi.json", app.base_url))
    .send()
    .await
    .unwrap()
    .json::<Value>()
    .await
    .unwrap();

  let mut checked = 0;
  for (method, path, _) in operations(&document) {
    if !path.starts_with("/api/v1/admin/") {
      continue;
    }
    for user in [&scene.eve, &scene.gina] {
      let (status, _) = send(&app, &method, &fill(&path, &scene), Some(&app.token(user))).await;
      assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "{method} {path} as {}",
        user.username
      );
    }
    checked += 1;
  }
  assert!(checked >= 14, "only {checked} admin operations found");
  app.stop().await;
}

#[tokio::test]
async fn outsiders_cannot_touch_conversations_spaces_or_meetings_they_are_not_part_of() {
  let Some(app) = start().await else { return };
  let scene = scene(&app).await;
  let document = app
    .http
    .get(format!("{}/api/v1/openapi.json", app.base_url))
    .send()
    .await
    .unwrap()
    .json::<Value>()
    .await
    .unwrap();

  let mut checked = 0;
  for (method, path, _) in operations(&document) {
    let scoped = ["{conversation_id}", "{space_id}", "{meeting_id}"]
      .iter()
      .any(|id| path.contains(id));
    if !scoped
      || path.starts_with("/api/v1/admin/")
      || OPEN_TO_MEMBERS.iter().any(|open| path.ends_with(open))
    {
      continue;
    }
    for user in [&scene.eve, &scene.gina] {
      let (status, body) = send(&app, &method, &fill(&path, &scene), Some(&app.token(user))).await;
      let ok = status.is_client_error() && status != StatusCode::UNAUTHORIZED
        || status == StatusCode::SERVICE_UNAVAILABLE && body["error_code"] == "unavailable";
      assert!(
        ok,
        "{method} {path} as outsider {}: {status} {body}",
        user.username
      );
    }
    checked += 1;
  }
  assert!(checked >= 40, "only {checked} scoped operations found");
  app.stop().await;
}

#[tokio::test]
async fn guests_cannot_create_spaces_or_bots_and_members_cannot_read_other_peoples_devices() {
  let Some(app) = start().await else { return };
  let scene = scene(&app).await;

  for (path, body) in [
    ("/spaces", json!({"name": "mine", "join_policy": "open"})),
    ("/admin/bots", json!({"name": "x"})),
  ] {
    let reply = app.post(&scene.gina, path, body).await;
    assert_eq!(
      reply.status,
      StatusCode::FORBIDDEN,
      "guest POST {path}: {}",
      reply.body
    );
  }

  // Revoking somebody else's device is not allowed.
  let revoked = app
    .delete(&scene.eve, &format!("/devices/{}", scene.alice_device))
    .await;
  assert!(revoked.status.is_client_error(), "{}", revoked.body);
  assert_eq!(
    app.get(&owner(), "/devices").await.body["devices"]
      .as_array()
      .unwrap()
      .len(),
    1
  );
  app.stop().await;
}
