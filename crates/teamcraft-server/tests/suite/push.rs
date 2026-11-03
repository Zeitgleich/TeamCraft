//! Push registration against a real server and Postgres.
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

/// A browser subscription on a public host (the key is the RFC 8291 example key).
const SUBSCRIPTION: &str = r#"{"endpoint":"https://push.example.com/send/abc","keys":{"p256dh":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4","auth":"BTBZMqHH6r4Tts7J_aSIgg"}}"#;

/// 32 bytes, base64.
const PUSH_KEY: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";

#[tokio::test]
async fn every_provider_can_be_registered_and_is_shown_without_secrets() {
  let app = app!();
  let carol = member("carol");
  let device = register_device(&app, &carol).await;
  for provider in ["apns", "fcm", "unifiedpush", "webpush"] {
    // UnifiedPush registrations use the Web Push subscription format.
    let token = if matches!(provider, "webpush" | "unifiedpush") {
      SUBSCRIPTION
    } else {
      "device-token"
    };
    let reply = app
      .put(
        &carol,
        &format!("/devices/{device}/push"),
        json!({"provider": provider, "token": token, "push_key": PUSH_KEY}),
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

#[tokio::test]
async fn web_push_subscriptions_are_checked_when_registered() {
  let app = app!();
  let carol = member("carol");
  let device = register_device(&app, &carol).await;
  let path = format!("/devices/{device}/push");
  for token in [
    "device-token",
    r#"{"endpoint":"http://push.example.com/x","keys":{"p256dh":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4","auth":"BTBZMqHH6r4Tts7J_aSIgg"}}"#,
    r#"{"endpoint":"https://169.254.169.254/x","keys":{"p256dh":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4","auth":"BTBZMqHH6r4Tts7J_aSIgg"}}"#,
    r#"{"endpoint":"https://push.example.com/x","keys":{"p256dh":"AAAA","auth":"AAAA"}}"#,
  ] {
    let reply = app
      .put(
        &carol,
        &path,
        json!({"provider": "webpush", "token": token, "push_key": PUSH_KEY}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{token}");
  }
  app.stop().await;
}

#[tokio::test]
async fn unified_push_registrations_are_checked_like_web_push() {
  let app = app!();
  let carol = member("carol");
  let device = register_device(&app, &carol).await;
  let path = format!("/devices/{device}/push");
  for token in [
    // A bare URL has no keys to encrypt with.
    "https://push.example.com/up/abc",
    r#"{"endpoint":"https://169.254.169.254/x","keys":{"p256dh":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4","auth":"BTBZMqHH6r4Tts7J_aSIgg"}}"#,
    r#"{"endpoint":"https://push.example.com/up/abc"}"#,
  ] {
    let reply = app
      .put(
        &carol,
        &path,
        json!({"provider": "unifiedpush", "token": token, "push_key": PUSH_KEY}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{token}");
  }
  app.stop().await;
}

mod subscription_change {
  use base64::Engine;
  use base64::engine::general_purpose::STANDARD;
  use reqwest::Method;
  use teamcraft_crypto::ids::DeviceId as DeviceIdCrypto;
  use teamcraft_crypto::push::subscription_change_mac;
  use teamcraft_crypto::secret::SymmetricKey;
  use teamcraft_server::testing::TestUser;

  use super::*;

  /// The same browser keys under a new endpoint, as after a rotation.
  const ROTATED: &str = r#"{"endpoint":"https://push.example.com/send/rotated","keys":{"p256dh":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4","auth":"BTBZMqHH6r4Tts7J_aSIgg"}}"#;
  const PATH: &str = "/push/web/subscription-change";

  #[tokio::test]
  async fn the_service_worker_replaces_the_subscription_without_a_token() {
    let app = app!();
    let carol = member("carol");
    let device = register_web_push(&app, &carol).await;
    let request = change(&device, SUBSCRIPTION, ROTATED, &push_key());
    let reply = app.call(Method::POST, None, PATH, Some(request.clone())).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    assert_eq!(registered_token(&app, &device).await, ROTATED);
    // A retry of the same request is fine; it changes nothing.
    let retry = app.call(Method::POST, None, PATH, Some(request)).await;
    assert_eq!(retry.status, StatusCode::NO_CONTENT, "{}", retry.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_proof_under_another_key_or_for_another_device_is_refused_alike() {
    let app = app!();
    let carol = member("carol");
    let device = register_web_push(&app, &carol).await;
    let wrong_key = SymmetricKey::from_bytes([9; 32]);
    let mut tampered = change(&device, SUBSCRIPTION, ROTATED, &push_key());
    tampered["token"] = json!(ROTATED.replace("rotated", "attacker"));
    for request in [
      change(&device, SUBSCRIPTION, ROTATED, &wrong_key),
      change("123456789", SUBSCRIPTION, ROTATED, &push_key()),
      tampered,
    ] {
      let reply = app.call(Method::POST, None, PATH, Some(request)).await;
      assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);
    }
    assert_eq!(registered_token(&app, &device).await, SUBSCRIPTION);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_change_from_a_subscription_that_was_replaced_meanwhile_conflicts() {
    let app = app!();
    let carol = member("carol");
    let device = register_web_push(&app, &carol).await;
    let first = change(&device, SUBSCRIPTION, ROTATED, &push_key());
    app.call(Method::POST, None, PATH, Some(first)).await;
    // A second worker run (or a replay) that still names the first subscription as current.
    let third = ROTATED.replace("rotated", "third");
    let stale = change(&device, SUBSCRIPTION, &third, &push_key());
    let reply = app.call(Method::POST, None, PATH, Some(stale)).await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    assert_eq!(registered_token(&app, &device).await, ROTATED);
    app.stop().await;
  }

  #[tokio::test]
  async fn only_web_push_registrations_and_valid_subscriptions_change() {
    let app = app!();
    let carol = member("carol");
    let device = register_device(&app, &carol).await;
    app
      .put(
        &carol,
        &format!("/devices/{device}/push"),
        json!({"provider": "fcm", "token": SUBSCRIPTION, "push_key": PUSH_KEY}),
      )
      .await;
    let request = change(&device, SUBSCRIPTION, ROTATED, &push_key());
    let reply = app.call(Method::POST, None, PATH, Some(request)).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.body);

    let device = register_web_push(&app, &carol).await;
    let private = r#"{"endpoint":"https://169.254.169.254/x","keys":{"p256dh":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4","auth":"BTBZMqHH6r4Tts7J_aSIgg"}}"#;
    let request = change(&device, SUBSCRIPTION, private, &push_key());
    let reply = app.call(Method::POST, None, PATH, Some(request)).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn empty_or_oversized_subscriptions_are_rejected_before_any_lookup() {
    let app = app!();
    let carol = member("carol");
    let device = register_web_push(&app, &carol).await;
    let huge = "x".repeat(5000);
    for (previous, token) in [
      ("", ROTATED),
      (SUBSCRIPTION, ""),
      (huge.as_str(), ROTATED),
      (SUBSCRIPTION, huge.as_str()),
    ] {
      let request = change(&device, previous, token, &push_key());
      let reply = app.call(Method::POST, None, PATH, Some(request)).await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    }
    assert_eq!(registered_token(&app, &device).await, SUBSCRIPTION);
    app.stop().await;
  }

  async fn register_web_push(app: &TestApp, user: &TestUser) -> String {
    let device = register_device(app, user).await;
    let reply = app
      .put(
        user,
        &format!("/devices/{device}/push"),
        json!({"provider": "webpush", "token": SUBSCRIPTION, "push_key": PUSH_KEY}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    device
  }

  fn change(device: &str, previous: &str, token: &str, key: &SymmetricKey) -> serde_json::Value {
    let id = DeviceIdCrypto::new(device.parse().unwrap());
    let mac = subscription_change_mac(key, id, previous, token);
    json!({
      "device_id": device,
      "previous_token": previous,
      "token": token,
      "mac": STANDARD.encode(mac),
    })
  }

  fn push_key() -> SymmetricKey {
    SymmetricKey::from_slice(&STANDARD.decode(PUSH_KEY).unwrap()).unwrap()
  }

  async fn registered_token(app: &TestApp, device: &str) -> String {
    teamcraft_db::testing::sqlx::query_scalar::<_, String>("SELECT push_token FROM devices WHERE id = $1")
      .bind(device.parse::<i64>().unwrap())
      .fetch_one(app.sql_pool())
      .await
      .unwrap()
  }
}
