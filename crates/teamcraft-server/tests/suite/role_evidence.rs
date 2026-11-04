//! Identity provider tokens on their way to the owners' apps (e2ee.md §3.1.3), against a real server
//! and Postgres. The server only keeps them; the apps verify them.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::{TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn token(user: &TestUser, issued_at: i64) -> String {
  let payload = URL_SAFE_NO_PAD.encode(
    serde_json::to_vec(&json!({"sub": user.subject, "iat": issued_at, "tc_identity_key": "1:ab"})).unwrap(),
  );
  format!("header.{payload}.signature")
}

async fn enroll(app: &TestApp, user: &TestUser) -> String {
  let id = user_id(app, user).await;
  let stored = app
    .put(
      user,
      "/me/identity-key",
      json!({
        "public_key": "a2V5MQ==",
        "signature": "c2ln",
        "key_bag": {"encrypted_key_bag": "YmFn", "wrapped_backup_keys": "d3JhcA=="},
      }),
    )
    .await;
  assert_eq!(stored.status, StatusCode::OK, "{}", stored.body);
  id
}

fn entries(reply: &Value) -> Vec<Value> {
  reply["entries"].as_array().unwrap().clone()
}

#[tokio::test]
async fn an_admins_token_reaches_the_owners_and_nobody_else() {
  let app = app!();
  let (alice, bob, carol) = (owner(), admin(), member("carol"));
  user_id(&app, &alice).await;
  let bob_id = enroll(&app, &bob).await;

  let published = app
    .put(&bob, "/me/role-evidence", json!({"token": token(&bob, 100)}))
    .await;
  assert_eq!(published.status, StatusCode::NO_CONTENT, "{}", published.body);

  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
  let entries = entries(&listed.body);
  assert_eq!(entries.len(), 1);
  assert_eq!(entries[0]["user_id"], bob_id);
  assert_eq!(entries[0]["token"], token(&bob, 100));
  assert_eq!(entries[0]["public_key"], "a2V5MQ==");

  // Tokens name people, with their e-mail address: only those who sign read them.
  for other in [&bob, &carol] {
    assert_eq!(
      app.get(other, "/role-evidence").await.status,
      StatusCode::FORBIDDEN
    );
  }
  app.stop().await;
}

#[tokio::test]
async fn somebody_elses_token_and_junk_are_refused() {
  let app = app!();
  let (bob, carol) = (admin(), member("carol"));
  enroll(&app, &bob).await;
  user_id(&app, &carol).await;

  let others = app
    .put(&carol, "/me/role-evidence", json!({"token": token(&bob, 100)}))
    .await;
  assert_eq!(others.status, StatusCode::FORBIDDEN, "{}", others.body);
  for junk in ["", "not-a-token", "a.!!.c", "a.e30.c"] {
    let reply = app.put(&bob, "/me/role-evidence", json!({"token": junk})).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{junk}: {}", reply.body);
  }
  let oversized = format!("h.{}.s", "x".repeat(16 * 1024));
  let reply = app
    .put(&bob, "/me/role-evidence", json!({"token": oversized}))
    .await;
  assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
  app.stop().await;
}

#[tokio::test]
async fn a_member_who_was_never_on_the_roster_has_nothing_to_store() {
  let app = app!();
  let (alice, carol) = (owner(), member("carol"));
  user_id(&app, &alice).await;
  enroll(&app, &carol).await;
  let reply = app
    .put(&carol, "/me/role-evidence", json!({"token": token(&carol, 100)}))
    .await;
  assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
  let listed = app.get(&alice, "/role-evidence").await;
  assert!(entries(&listed.body).is_empty());
  app.stop().await;
}

#[tokio::test]
async fn an_older_token_does_not_replace_a_newer_one() {
  let app = app!();
  let (alice, bob) = (owner(), admin());
  user_id(&app, &alice).await;
  enroll(&app, &bob).await;
  for issued_at in [200, 100] {
    let reply = app
      .put(
        &bob,
        "/me/role-evidence",
        json!({"token": token(&bob, issued_at)}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
  }
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(entries(&listed.body)[0]["token"], token(&bob, 200));
  app.stop().await;
}

#[tokio::test]
async fn people_who_lost_role_or_account_are_reported_for_the_roster_only() {
  let app = app!();
  let (alice, bob, carol) = (owner(), admin(), member("carol"));
  let alice_id = enroll(&app, &alice).await;
  let bob_id = user_id(&app, &bob).await;
  let carol_id = user_id(&app, &carol).await;
  let device = register_device(&app, &alice).await;
  // Nothing signed, nothing to report.
  let empty = app.get(&alice, "/role-evidence").await;
  assert_eq!(empty.body["withdrawn"], json!([]));

  let roster = serde_json::to_vec(&json!({
    "version": 1,
    "members": [
      {"user_id": alice_id, "role": "owner", "identity_key": "a2V5MQ=="},
      {"user_id": bob_id, "role": "admin", "identity_key": "a2V5MQ=="},
      {"user_id": carol_id, "role": "admin", "identity_key": "a2V5MQ=="},
    ],
  }))
  .unwrap();
  let appended = app
    .post(
      &alice,
      "/workspace/roster",
      json!({
        "expected_version": 0,
        "payload": base64::engine::general_purpose::STANDARD.encode(&roster),
        "signature": "c2ln",
        "signer_device_id": device,
      }),
    )
    .await;
  assert!(appended.status.is_success(), "{}", appended.body);

  // Carol is signed as admin and only a member by the server's records. Bob holds his role.
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(
    listed.body["withdrawn"],
    json!([{"user_id": carol_id, "reason": "role_gone"}])
  );
  app.stop().await;
}
