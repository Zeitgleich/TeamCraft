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
