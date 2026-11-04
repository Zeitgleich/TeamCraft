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
async fn a_token_with_no_role_left_is_kept_and_handed_out_once_expired() {
  let app = app!();
  let (alice, bob) = (owner(), admin());
  user_id(&app, &alice).await;
  let bob_id = enroll(&app, &bob).await;

  // Keycloak then signs a token without any TeamCraft role: the server refuses it, and keeps it.
  let no_role = TestUser {
    roles: vec!["offline_access".to_owned()],
    ..admin()
  };
  let refused = app.get(&no_role, "/me").await;
  assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
  // Still valid: not handed out, as it is a credential until it expires.
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"], json!([]));

  // By now it has expired (the test moves the row, not the clock).
  sqlx::query("UPDATE role_losses SET expires_at = 1")
    .execute(app.sql_pool())
    .await
    .unwrap();
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"][0]["user_id"], bob_id);

  // The role comes back, and Bob's app publishes that: it is over.
  let newer = time::OffsetDateTime::now_utc().unix_timestamp() + 60;
  let published = app
    .put(&bob, "/me/role-evidence", json!({"token": token(&bob, newer)}))
    .await;
  assert_eq!(published.status, StatusCode::NO_CONTENT, "{}", published.body);
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"], json!([]), "bob holds his role again");
  app.stop().await;
}

#[tokio::test]
async fn the_role_returning_before_anything_is_signed_ends_the_loss() {
  // A refused token changes nothing in the server's records, so nothing but the token itself tells.
  let app = app!();
  let (alice, bob) = (owner(), admin());
  user_id(&app, &alice).await;
  enroll(&app, &bob).await;
  let no_role = TestUser {
    roles: vec!["offline_access".to_owned()],
    ..admin()
  };
  assert_eq!(app.get(&no_role, "/me").await.status, StatusCode::FORBIDDEN);
  // An accepted token with the role, same as before as far as the records go.
  assert_eq!(app.get(&bob, "/me").await.status, StatusCode::OK);
  sqlx::query("UPDATE role_losses SET expires_at = 1")
    .execute(app.sql_pool())
    .await
    .unwrap();
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"], json!([]));
  app.stop().await;
}

#[tokio::test]
async fn an_owner_demoted_to_admin_stays_on_file_while_they_hold_admin() {
  let app = app!();
  let alice = owner();
  let dora = TestUser::new("dora", &["owner"]);
  user_id(&app, &alice).await;
  enroll(&app, &dora).await;
  let demoted = TestUser {
    roles: vec!["admin".to_owned()],
    ..dora.clone()
  };
  assert_eq!(app.get(&demoted, "/me").await.status, StatusCode::OK);
  assert_eq!(app.get(&demoted, "/me").await.status, StatusCode::OK);
  sqlx::query("UPDATE role_losses SET expires_at = 1")
    .execute(app.sql_pool())
    .await
    .unwrap();
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"].as_array().unwrap().len(), 1);
  // The owner role coming back ends it.
  assert_eq!(app.get(&dora, "/me").await.status, StatusCode::OK);
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"], json!([]));
  app.stop().await;
}

#[tokio::test]
async fn a_loss_is_noticed_also_when_the_provider_sync_got_there_first() {
  let app = app!();
  let (alice, bob) = (owner(), admin());
  user_id(&app, &alice).await;
  let bob_id = enroll(&app, &bob).await;
  // What the sync does when the provider says Bob has a lower role.
  sqlx::query("UPDATE users SET oidc_role = 'member' WHERE id = $1")
    .bind(bob_id.parse::<i64>().unwrap())
    .execute(app.sql_pool())
    .await
    .unwrap();
  let demoted = TestUser {
    roles: vec!["member".to_owned()],
    ..admin()
  };
  assert_eq!(app.get(&demoted, "/me").await.status, StatusCode::OK);
  sqlx::query("UPDATE role_losses SET expires_at = 1")
    .execute(app.sql_pool())
    .await
    .unwrap();
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"][0]["user_id"], bob_id);
  app.stop().await;
}

#[tokio::test]
async fn a_lower_role_on_a_request_is_kept_too_and_a_member_never_was_anything_to_lose() {
  let app = app!();
  let (alice, bob) = (owner(), admin());
  user_id(&app, &alice).await;
  enroll(&app, &bob).await;
  let demoted = TestUser {
    roles: vec!["member".to_owned()],
    ..admin()
  };
  assert_eq!(app.get(&demoted, "/me").await.status, StatusCode::OK);
  sqlx::query("UPDATE role_losses SET expires_at = 1")
    .execute(app.sql_pool())
    .await
    .unwrap();
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"].as_array().unwrap().len(), 1);

  // Somebody who never had a role to lose leaves nothing behind.
  let carol = member("carol");
  user_id(&app, &carol).await;
  let refused = TestUser {
    roles: vec!["offline_access".to_owned()],
    ..member("carol")
  };
  assert_eq!(app.get(&refused, "/me").await.status, StatusCode::FORBIDDEN);
  let listed = app.get(&alice, "/role-evidence").await;
  assert_eq!(listed.body["role_losses"].as_array().unwrap().len(), 1);
  app.stop().await;
}

#[tokio::test]
async fn a_roster_that_took_tokens_into_account_drops_them() {
  let app = app!();
  let (alice, bob) = (owner(), admin());
  let alice_id = enroll(&app, &alice).await;
  let bob_id = enroll(&app, &bob).await;
  let device = register_device(&app, &alice).await;
  let published = app
    .put(&bob, "/me/role-evidence", json!({"token": token(&bob, 100)}))
    .await;
  assert_eq!(published.status, StatusCode::NO_CONTENT, "{}", published.body);

  let roster = serde_json::to_vec(&json!({
    "version": 1,
    "members": [{"user_id": alice_id, "role": "owner", "identity_key": "a2V5MQ=="}],
    "provider_marks": { bob_id.as_str(): {"sub": "sub-bob", "issued_at": 100} },
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
  let listed = app.get(&alice, "/role-evidence").await;
  assert!(entries(&listed.body).is_empty(), "{}", listed.body);
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
