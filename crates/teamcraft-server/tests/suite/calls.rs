//! Call join tokens against a real server and Postgres (`--features integration`, needs `DATABASE_URL`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::{CALL_JOIN_TOKEN_AUDIENCE, CallJoinClaims};
use teamcraft_server::testing::{TEST_ISSUER, TestApp};

/// Throwaway Ed25519 key pair used only by these tests. It protects nothing.
pub(crate) const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
const PUBLIC_KEY_PEM: &str = "-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEA8YVjY2Zv9XPhv003tFAgzW7J6gfDn+ICj5xCB2LXMVg=
-----END PUBLIC KEY-----
";
const SFU_URL: &str = "https://sfu.example.com:4433/call";

macro_rules! app {
  () => {
    match TestApp::start_with_calls(SIGNING_KEY_PEM, SFU_URL).await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn members_get_tokens_for_the_same_call() {
  let app = app!();
  let alice = owner();
  let bob = member("bob");
  let channel = channel_with(&app, &[&bob]).await;
  let alice_device = register_device(&app, &alice).await;
  let bob_device = register_device(&app, &bob).await;

  let alice_reply = request_token(&app, &alice, &channel, &alice_device).await;
  let bob_reply = request_token(&app, &bob, &channel, &bob_device).await;

  assert_eq!(alice_reply.status, StatusCode::OK, "{}", alice_reply.body);
  assert_eq!(bob_reply.status, StatusCode::OK, "{}", bob_reply.body);
  assert_eq!(alice_reply.body["call_id"], bob_reply.body["call_id"]);
  assert_eq!(alice_reply.body["sfu_url"], SFU_URL);
  let claims = verify(alice_reply.body["token"].as_str().unwrap());
  assert_eq!(
    claims.call_id.to_string(),
    alice_reply.body["call_id"].as_str().unwrap()
  );
  assert_eq!(claims.device_id.to_string(), alice_device);
  assert_eq!(claims.user_id.to_string(), user_id(&app, &alice).await);
  assert_eq!(
    alice_reply.body["expires_at"].as_i64().unwrap(),
    i64::try_from(claims.exp).unwrap() * 1000
  );
  app.stop().await;
}

#[tokio::test]
async fn non_members_get_no_token() {
  let app = app!();
  let channel = channel_with(&app, &[]).await;
  let eve = member("eve");
  let eve_device = register_device(&app, &eve).await;

  let reply = request_token(&app, &eve, &channel, &eve_device).await;

  assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
  app.stop().await;
}

#[tokio::test]
async fn token_needs_one_of_the_callers_own_devices() {
  let app = app!();
  let alice = owner();
  let bob = member("bob");
  let channel = channel_with(&app, &[&bob]).await;
  let bob_device = register_device(&app, &bob).await;

  let reply = request_token(&app, &alice, &channel, &bob_device).await;

  assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
  assert_eq!(reply.error_code(), "device_not_found");
  app.stop().await;
}

#[tokio::test]
async fn calls_without_configuration_are_unavailable() {
  let Some(app) = TestApp::start().await else {
    return;
  };
  let alice = owner();
  let channel = channel_with(&app, &[]).await;
  let device = register_device(&app, &alice).await;

  let reply = request_token(&app, &alice, &channel, &device).await;

  assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
  assert_eq!(reply.error_code(), "unavailable");
  app.stop().await;
}

async fn request_token(
  app: &TestApp,
  user: &teamcraft_server::testing::TestUser,
  conversation: &str,
  device: &str,
) -> teamcraft_server::testing::Reply {
  app
    .post(
      user,
      &format!("/conversations/{conversation}/call/join-token"),
      json!({ "device_id": device }),
    )
    .await
}

fn verify(token: &str) -> CallJoinClaims {
  let mut validation = Validation::new(Algorithm::EdDSA);
  validation.set_audience(&[CALL_JOIN_TOKEN_AUDIENCE]);
  validation.set_issuer(&[TEST_ISSUER]);
  jsonwebtoken::decode::<CallJoinClaims>(
    token,
    &DecodingKey::from_ed_pem(PUBLIC_KEY_PEM.as_bytes()).unwrap(),
    &validation,
  )
  .unwrap()
  .claims
}
