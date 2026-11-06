//! Identity keys and device certificates against a real server and Postgres.
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

fn key(public: &str) -> serde_json::Value {
  json!({
    "public_key": public,
    "signature": "c2ln",
    "attestation": "eyJhbGciOiJSUzI1NiJ9.token.sig",
    "key_bag": {"encrypted_key_bag": "YmFn", "wrapped_backup_keys": "d3JhcA=="},
  })
}

#[tokio::test]
async fn an_identity_key_is_stored_with_its_attestation_and_served_to_others() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  assert_eq!(
    app
      .get(&dave, &format!("/users/{carol_id}/identity-key"))
      .await
      .error_code(),
    "identity_key_not_found"
  );

  let stored = app.put(&carol, "/me/identity-key", key("a2V5MQ==")).await;
  assert_eq!(stored.status, StatusCode::OK, "{}", stored.body);
  let fetched = app.get(&dave, &format!("/users/{carol_id}/identity-key")).await;
  assert_eq!(fetched.body["public_key"], "a2V5MQ==");
  assert_eq!(fetched.body["attestation"], "eyJhbGciOiJSUzI1NiJ9.token.sig");
  assert_eq!(fetched.body["user_id"], carol_id);
  app.stop().await;
}

#[tokio::test]
async fn the_enrollment_of_a_new_user_is_empty_not_missing() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  let before = app.get(&dave, &format!("/users/{carol_id}/enrollment")).await;
  assert_eq!(before.status, StatusCode::OK, "{}", before.body);
  assert_eq!(before.body, json!({}));

  app.put(&carol, "/me/identity-key", key("a2V5MQ==")).await;
  let list = json!({"expected_sequence": 0, "sequence": 1, "signed_list": "AQID"});
  assert_eq!(
    app.put(&carol, "/me/device-list", list).await.status,
    StatusCode::OK
  );
  let after = app.get(&dave, &format!("/users/{carol_id}/enrollment")).await;
  assert_eq!(after.body["identity_key"]["public_key"], "a2V5MQ==");
  assert_eq!(after.body["identity_key"]["user_id"], carol_id);
  assert_eq!(
    after.body["device_list"],
    json!({"sequence": 1, "signed_list": "AQID"})
  );
  assert_eq!(
    app.get(&dave, "/users/999999/enrollment").await.error_code(),
    "user_not_found"
  );
  app.stop().await;
}

#[tokio::test]
async fn an_identity_key_is_stored_only_with_its_key_bag() {
  let app = app!();
  let carol = member("carol");
  let carol_id = user_id(&app, &carol).await;
  let mut bare = key("a2V5MQ==");
  bare.as_object_mut().unwrap().remove("key_bag");
  let refused = app.put(&carol, "/me/identity-key", bare).await;
  assert_eq!(refused.status, StatusCode::BAD_REQUEST);
  let mut empty_bag = key("a2V5MQ==");
  empty_bag["key_bag"]["encrypted_key_bag"] = json!("");
  assert_eq!(
    app.put(&carol, "/me/identity-key", empty_bag).await.status,
    StatusCode::BAD_REQUEST
  );
  assert_eq!(
    app
      .get(&carol, &format!("/users/{carol_id}/identity-key"))
      .await
      .error_code(),
    "identity_key_not_found",
    "nothing stored by a refused upload"
  );

  let stored = app.put(&carol, "/me/identity-key", key("a2V5MQ==")).await;
  assert_eq!(stored.body["key_bag_version"], 1);
  let bag = app.get(&carol, "/me/key-bag").await;
  assert_eq!(bag.body["version"], 1);
  assert_eq!(bag.body["encrypted_key_bag"], "YmFn");
  assert_eq!(bag.body["wrapped_backup_keys"], "d3JhcA==");
  // A losing second upload changes neither the key nor the bag.
  let mut other = key("a2V5Mg==");
  other["key_bag"]["encrypted_key_bag"] = json!("b3RoZXI=");
  app.put(&carol, "/me/identity-key", other).await;
  assert_eq!(
    app.get(&carol, "/me/key-bag").await.body["encrypted_key_bag"],
    "YmFn"
  );
  app.stop().await;
}

#[tokio::test]
async fn replacing_a_key_needs_an_explicit_reset() {
  let app = app!();
  let carol = member("carol");
  let carol_id = user_id(&app, &carol).await;
  app.put(&carol, "/me/identity-key", key("a2V5MQ==")).await;

  let again = app.put(&carol, "/me/identity-key", key("a2V5Mg==")).await;
  assert_eq!(again.status, StatusCode::CONFLICT);
  let mut reset = key("a2V5Mg==");
  reset["replace"] = json!(true);
  assert_eq!(
    app.put(&carol, "/me/identity-key", reset).await.status,
    StatusCode::OK
  );
  let current = app.get(&carol, &format!("/users/{carol_id}/identity-key")).await;
  assert_eq!(current.body["public_key"], "a2V5Mg==");
  app.stop().await;
}

#[tokio::test]
async fn invalid_key_material_is_rejected() {
  let app = app!();
  let carol = member("carol");
  let empty = app
    .put(
      &carol,
      "/me/identity-key",
      json!({"public_key": "", "signature": "c2ln"}),
    )
    .await;
  assert_eq!(empty.status, StatusCode::BAD_REQUEST);
  let huge = "A".repeat(8000);
  let too_big = app
    .put(
      &carol,
      "/me/identity-key",
      json!({"public_key": huge, "signature": "c2ln"}),
    )
    .await;
  assert_eq!(too_big.status, StatusCode::BAD_REQUEST);
  app.stop().await;
}

#[tokio::test]
async fn guests_only_fetch_keys_of_people_they_share_a_conversation_with() {
  let app = app!();
  let (carol, dave, ext) = (member("carol"), member("dave"), guest("ext"));
  let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
  user_id(&app, &ext).await;
  app.put(&carol, "/me/identity-key", key("a2V5MQ==")).await;
  app.put(&dave, "/me/identity-key", key("a2V5Mg==")).await;

  let alice = owner();
  let space = create_space(&app, &alice, "Team", "invite_only").await;
  let ext_id = user_id(&app, &ext).await;
  add_space_member(&app, &alice, &space, &carol_id, "member").await;
  add_space_member(&app, &alice, &space, &ext_id, "guest").await;
  let channel = create_channel(&app, &alice, &space, "client", "public").await;
  add_channel_member(&app, &alice, &channel, &carol_id).await;
  add_channel_member(&app, &alice, &channel, &ext_id).await;

  assert_eq!(
    app
      .get(&ext, &format!("/users/{carol_id}/identity-key"))
      .await
      .status,
    StatusCode::OK
  );
  assert_eq!(
    app
      .get(&ext, &format!("/users/{dave_id}/identity-key"))
      .await
      .error_code(),
    "user_not_found"
  );
  assert_eq!(
    app.get(&ext, &format!("/users/{dave_id}/devices")).await.status,
    StatusCode::NOT_FOUND
  );
  app.stop().await;
}

#[tokio::test]
async fn device_keys_come_with_their_cross_signatures() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  let device = register_device(&app, &carol).await;

  let listed = app.get(&dave, &format!("/users/{carol_id}/devices")).await;
  let first = &listed.body["devices"][0];
  assert_eq!(first["id"], device);
  assert_eq!(first["public_key"], "cHVibGlj");
  assert_eq!(first["cross_signature"], "c2ln");
  // Revoked devices disappear.
  app.delete(&carol, &format!("/devices/{device}")).await;
  assert_eq!(
    app.get(&dave, &format!("/users/{carol_id}/devices")).await.body["devices"]
      .as_array()
      .unwrap()
      .len(),
    0
  );
  app.stop().await;
}

mod attestation_of_a_published_key {
  use base64::Engine as _;
  use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
  use teamcraft_crypto::attestation::attestation_claim;
  use teamcraft_crypto::identity::IdentityPublicKey;
  use teamcraft_server::testing::TestUser;

  use super::*;

  const PUBLIC_KEY: [u8; 32] = [7; 32];

  #[tokio::test]
  async fn a_token_naming_the_caller_and_their_key_is_stored_with_it() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = enroll(&app, &carol).await;
    let token = token(&carol, &claim(&carol_id, PUBLIC_KEY));

    let stored = app
      .put(&carol, "/me/identity-key/attestation", json!({"token": token}))
      .await;

    assert_eq!(stored.status, StatusCode::NO_CONTENT, "{}", stored.body);
    let served = app.get(&dave, &format!("/users/{carol_id}/identity-key")).await;
    assert_eq!(served.body["attestation"], token);
    assert_eq!(served.body["public_key"], STANDARD.encode(PUBLIC_KEY));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_token_of_somebody_else_is_refused() {
    let app = app!();
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = enroll(&app, &carol).await;
    user_id(&app, &dave).await;

    let refused = app
      .put(
        &carol,
        "/me/identity-key/attestation",
        json!({"token": token(&dave, &claim(&carol_id, PUBLIC_KEY))}),
      )
      .await;

    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_token_for_another_key_or_no_token_is_refused() {
    let app = app!();
    let carol = member("carol");
    let carol_id = enroll(&app, &carol).await;

    for body in [
      json!({"token": token(&carol, &claim(&carol_id, [8; 32]))}),
      json!({"token": "not-a-token"}),
      json!({"token": "x".repeat(17 * 1024)}),
    ] {
      let refused = app.put(&carol, "/me/identity-key/attestation", body).await;
      assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    }
    let served = app.get(&carol, &format!("/users/{carol_id}/identity-key")).await;
    assert!(served.body.get("attestation").is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn without_a_published_key_there_is_nothing_to_attest() {
    let app = app!();
    let carol = member("carol");
    let carol_id = user_id(&app, &carol).await;

    let refused = app
      .put(
        &carol,
        "/me/identity-key/attestation",
        json!({"token": token(&carol, &claim(&carol_id, PUBLIC_KEY))}),
      )
      .await;

    assert_eq!(refused.error_code(), "identity_key_not_found");
    app.stop().await;
  }

  async fn enroll(app: &TestApp, user: &TestUser) -> String {
    let id = user_id(app, user).await;
    let mut body = key(&STANDARD.encode(PUBLIC_KEY));
    body.as_object_mut().unwrap().remove("attestation");
    let stored = app.put(user, "/me/identity-key", body).await;
    assert_eq!(stored.status, StatusCode::OK, "{}", stored.body);
    id
  }

  fn claim(user_id: &str, public_key: [u8; 32]) -> String {
    attestation_claim(
      teamcraft_crypto::ids::UserId::new(user_id.parse().unwrap()),
      &IdentityPublicKey::from_bytes(public_key),
    )
  }

  /// Unsigned: the server doesn't verify tokens, the apps do.
  fn token(user: &TestUser, claim: &str) -> String {
    let payload = URL_SAFE_NO_PAD.encode(
      serde_json::to_vec(&json!({"sub": user.subject, "iat": 100, "tc_identity_key": claim})).unwrap(),
    );
    format!("header.{payload}.signature")
  }
}
