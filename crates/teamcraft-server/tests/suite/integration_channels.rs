//! Integration channels: flag, log-suited retention and the no-push default (ARCHITECTURE.md §4.5).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

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

async fn create(app: &TestApp, body: serde_json::Value) -> (String, serde_json::Value) {
  let alice = owner();
  let space = create_space(app, &alice, "Ops", "invite_only").await;
  let mut request = json!({"name": "logs", "visibility": "private"});
  for (key, value) in body.as_object().unwrap() {
    request[key] = value.clone();
  }
  let reply = app
    .post(&alice, &format!("/spaces/{space}/channels"), request)
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  (reply.id(), reply.body)
}

#[tokio::test]
async fn an_integration_channel_gets_log_retention_unless_it_has_its_own() {
  let app = app!();
  let (plain, plain_body) = create(&app, json!({})).await;
  assert_eq!(plain_body["is_integration"], false);
  let alice = owner();
  assert_eq!(
    app
      .get(&alice, &format!("/conversations/{plain}/retention"))
      .await
      .body["own"],
    json!({})
  );

  let (flagged, body) = create(&app, json!({"is_integration": true})).await;
  assert_eq!(body["is_integration"], true);
  let retention = app
    .get(&alice, &format!("/conversations/{flagged}/retention"))
    .await;
  assert_eq!(retention.body["own"]["max_age_days"], 30, "{}", retention.body);

  // Flagging an existing channel applies the default too, but not over an own setting.
  let patched = app
    .patch(
      &alice,
      &format!("/conversations/{plain}"),
      json!({"is_integration": true}),
    )
    .await;
  assert_eq!(patched.status, StatusCode::OK, "{}", patched.body);
  assert_eq!(patched.body["is_integration"], true);
  assert_eq!(
    app
      .get(&alice, &format!("/conversations/{plain}/retention"))
      .await
      .body["own"]["max_age_days"],
    30
  );
  let (own, _) = create(&app, json!({})).await;
  app
    .put(
      &alice,
      &format!("/conversations/{own}/retention"),
      json!({"retention": {"max_age_days": 7}}),
    )
    .await;
  app
    .patch(
      &alice,
      &format!("/conversations/{own}"),
      json!({"is_integration": true}),
    )
    .await;
  assert_eq!(
    app
      .get(&alice, &format!("/conversations/{own}/retention"))
      .await
      .body["own"]["max_age_days"],
    7
  );
  app.stop().await;
}

#[tokio::test]
async fn an_integration_channel_cannot_be_public() {
  let app = app!();
  let alice = owner();
  let space = create_space(&app, &alice, "Ops", "invite_only").await;
  let created = app
    .post(
      &alice,
      &format!("/spaces/{space}/channels"),
      json!({"name": "logs", "visibility": "public", "is_integration": true}),
    )
    .await;
  assert_eq!(created.status, StatusCode::CONFLICT, "{}", created.body);

  let public = create_channel(&app, &alice, &space, "open", "public").await;
  let flagged = app
    .patch(
      &alice,
      &format!("/conversations/{public}"),
      json!({"is_integration": true}),
    )
    .await;
  assert_eq!(flagged.status, StatusCode::CONFLICT, "{}", flagged.body);
}

#[tokio::test]
async fn a_channel_admin_without_space_rights_cannot_bring_a_bot_into_the_space() {
  let app = app!();
  let alice = owner();
  let bob = member("bob");
  let bob_id = user_id(&app, &bob).await;
  let space = create_space(&app, &alice, "Ops", "invite_only").await;
  add_space_member(&app, &alice, &space, &bob_id, "member").await;
  let channel = create_channel(&app, &alice, &space, "logs", "private").await;
  add_channel_member(&app, &alice, &channel, &bob_id).await;
  let promoted = app
    .patch(
      &alice,
      &format!("/conversations/{channel}/members/{bob_id}"),
      json!({"role": "admin"}),
    )
    .await;
  assert_eq!(promoted.status, StatusCode::NO_CONTENT, "{}", promoted.body);
  let bot = app
    .post(&admin(), "/admin/bots", json!({"name": "Log bridge"}))
    .await
    .body["bot"]["id"]
    .as_str()
    .unwrap()
    .to_owned();

  // The channel add needs the bot in the space; the space add is the space admins' (or inviters') call.
  let direct = app
    .post(
      &bob,
      &format!("/conversations/{channel}/members"),
      json!({"user_id": bot}),
    )
    .await;
  assert!(!direct.status.is_success(), "{} {}", direct.status, direct.body);
  let into_space = app
    .post(
      &bob,
      &format!("/spaces/{space}/members"),
      json!({"user_id": bot, "role": "guest"}),
    )
    .await;
  assert_eq!(into_space.status, StatusCode::FORBIDDEN, "{}", into_space.body);
  let members = app
    .get(&alice, &format!("/conversations/{channel}/members"))
    .await;
  assert!(
    !members.body.to_string().contains(&bot),
    "the bot is not in the channel"
  );
}
