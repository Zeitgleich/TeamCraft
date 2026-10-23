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
