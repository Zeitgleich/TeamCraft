//! Push registration against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

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

/// 32 bytes, base64.
const PUSH_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

#[tokio::test]
async fn every_provider_can_be_registered_and_is_shown_without_secrets() {
  let app = app!();
  let carol = member("carol");
  let device = register_device(&app, &carol).await;
  for provider in ["apns", "fcm", "unifiedpush", "webpush"] {
    let reply = app
      .put(
        &carol,
        &format!("/devices/{device}/push"),
        json!({"provider": provider, "token": "device-token", "push_key": PUSH_KEY}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{provider}: {}", reply.body);
    let listed = app.get(&carol, "/devices").await;
    assert_eq!(listed.body["devices"][0]["push_provider"], provider);
    let text = listed.body.to_string();
    assert!(
      !text.contains("device-token") && !text.contains(PUSH_KEY),
      "secrets leaked: {text}"
    );
  }
  app.stop().await;
}

#[tokio::test]
async fn registrations_can_be_removed_and_belong_to_the_owner() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let device = register_device(&app, &carol).await;
  let body = json!({"provider": "fcm", "token": "t", "push_key": PUSH_KEY});

  assert_eq!(
    app
      .put(&dave, &format!("/devices/{device}/push"), body.clone())
      .await
      .error_code(),
    "device_not_found"
  );
  app.put(&carol, &format!("/devices/{device}/push"), body).await;
  assert_eq!(
    app.delete(&dave, &format!("/devices/{device}/push")).await.status,
    StatusCode::NOT_FOUND
  );
  assert_eq!(
    app
      .delete(&carol, &format!("/devices/{device}/push"))
      .await
      .status,
    StatusCode::NO_CONTENT
  );
  assert!(
    app.get(&carol, "/devices").await.body["devices"][0]
      .get("push_provider")
      .is_none()
  );
  app.stop().await;
}

#[tokio::test]
async fn invalid_registrations_are_rejected() {
  let app = app!();
  let carol = member("carol");
  let device = register_device(&app, &carol).await;
  let path = format!("/devices/{device}/push");
  for body in [
    json!({"provider": "carrier-pigeon", "token": "t", "push_key": PUSH_KEY}),
    json!({"provider": "fcm", "token": "", "push_key": PUSH_KEY}),
    json!({"provider": "fcm", "token": "t", "push_key": "c2hvcnQ="}),
    json!({"provider": "fcm", "token": "t".repeat(5000), "push_key": PUSH_KEY}),
  ] {
    assert_eq!(
      app.put(&carol, &path, body.clone()).await.status,
      StatusCode::BAD_REQUEST,
      "{body}"
    );
  }
  app.stop().await;
}
