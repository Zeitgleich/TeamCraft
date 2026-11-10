//! The caller's own records for a data export (issue #211): `GET /me/personal-data`, against a real
//! server and Postgres. Only the caller's own data, for every role.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn a_member_gets_their_own_account_devices_settings_and_memberships() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  let dave_id = user_id(&app, &dave).await;
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let carol_device = register_device(&app, &carol).await;
  let dave_device = register_device(&app, &dave).await;
  let put = app
    .put(
      &carol,
      "/me/profile",
      json!({"title": "Engineer", "time_zone": "Europe/Berlin"}),
    )
    .await;
  assert_eq!(put.status, StatusCode::OK, "{}", put.body);

  let reply = app.get(&carol, "/me/personal-data").await;

  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  let data = &reply.body;
  assert_eq!(data["version"], 1);
  assert_eq!(data["account"]["id"], carol_id.as_str());
  assert_eq!(data["account"]["kind"], "member");
  assert_eq!(data["account"]["role"], "member");
  assert_eq!(data["profile"]["title"], "Engineer");
  assert!(data["notification_settings"].is_object());
  assert_eq!(ids(&data["devices"]), vec![carol_device.clone()]);
  assert!(
    ids(&data["conversations"]).contains(&channel),
    "the channel Carol is in: {}",
    data["conversations"]
  );
  assert_eq!(data["spaces"].as_array().unwrap().len(), 1);
  // Nothing of Dave's: not his device, not his account.
  let text = data.to_string();
  assert!(!text.contains(&dave_device), "another user's device leaked");
  assert_ne!(data["account"]["id"], dave_id.as_str());
  // Ciphertext and keys stay out: only that a key bag exists.
  assert!(data.get("key_bag").is_none(), "no key bag stored yet");
  app.stop().await;
}

#[tokio::test]
async fn the_key_bag_and_scheduled_messages_show_as_metadata_only() {
  let app = app!();
  let carol = member("carol");
  user_id(&app, &carol).await;
  let stored = app
    .put(
      &carol,
      "/me/key-bag",
      json!({"expected_version": 0, "encrypted_key_bag": "c2VjcmV0", "wrapped_backup_keys": "d3JhcA=="}),
    )
    .await;
  assert_eq!(stored.status, StatusCode::OK, "{}", stored.body);

  let reply = app.get(&carol, "/me/personal-data").await;

  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  let bag = &reply.body["key_bag"];
  assert_eq!(bag["version"], 1);
  assert_eq!(bag["size_bytes"], 10, "six and four bytes");
  assert!(bag.get("encrypted_key_bag").is_none());
  assert!(!reply.body.to_string().contains("c2VjcmV0"));
  app.stop().await;
}

#[tokio::test]
async fn a_guest_and_an_admin_get_only_their_own_data_too() {
  let app = app!();
  let (bob, erin, carol) = (admin(), guest("erin"), member("carol"));
  let bob_id = user_id(&app, &bob).await;
  let erin_id = user_id(&app, &erin).await;
  user_id(&app, &carol).await;
  let carol_device = register_device(&app, &carol).await;
  let erin_device = register_device(&app, &erin).await;

  let guest = app.get(&erin, "/me/personal-data").await;
  assert_eq!(guest.status, StatusCode::OK, "{}", guest.body);
  assert_eq!(guest.body["account"]["id"], erin_id.as_str());
  assert_eq!(guest.body["account"]["kind"], "guest");
  assert_eq!(ids(&guest.body["devices"]), vec![erin_device]);
  assert!(!guest.body.to_string().contains(&carol_device));

  let own = app.get(&bob, "/me/personal-data").await;
  assert_eq!(own.status, StatusCode::OK, "{}", own.body);
  assert_eq!(own.body["account"]["id"], bob_id.as_str());
  assert_eq!(own.body["account"]["role"], "admin");
  assert!(
    own.body["devices"].as_array().unwrap().is_empty(),
    "an admin's export holds no one else's devices"
  );
  assert!(!own.body.to_string().contains(&carol_device));
  app.stop().await;
}

#[tokio::test]
async fn without_a_token_nothing_is_answered() {
  let app = app!();
  let reply = app
    .http
    .get(format!("{}/api/v1/me/personal-data", app.base_url))
    .send()
    .await
    .unwrap();
  assert_eq!(reply.status(), StatusCode::UNAUTHORIZED);
  app.stop().await;
}

fn ids(list: &Value) -> Vec<String> {
  list
    .as_array()
    .unwrap()
    .iter()
    .map(|item| item["id"].as_str().unwrap().to_owned())
    .collect()
}
