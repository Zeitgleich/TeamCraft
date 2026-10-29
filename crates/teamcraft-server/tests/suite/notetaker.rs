//! The notetaker's API key and policy checks (e2ee.md §8.2, admin.md §3.7): admins store the key
//! write-only, only the bot account of the enabled signed policy gets it, and owners can't publish
//! a malformed notetaker group.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use teamcraft_proto::{NotetakerPolicy, PolicyDocument, UserId, WorkspacePolicy};
use teamcraft_server::testing::TestApp;

const KEY: &str = "sk-or-v1-notetaker-test";
const KEY_SHA256: &str = "0000000000000000000000000000000000000000000000000000000000000000";

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

async fn create_bot(app: &TestApp, name: &str) -> (UserId, String) {
  let reply = app.post(&admin(), "/admin/bots", json!({"name": name})).await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  (
    reply.body["bot"]["id"].as_str().unwrap().parse().unwrap(),
    reply.body["token"].as_str().unwrap().to_owned(),
  )
}

/// Puts the server's copy of the signed policy in force directly (the signature is the clients'
/// business; the server only reads the policy).
fn enable_notetaker(app: &TestApp, bot: UserId) {
  let mut policy = app.state.policy.get().as_ref().clone();
  policy.notetaker = NotetakerPolicy {
    is_enabled: true,
    maybe_bot: Some(bot),
    maybe_api_key_sha256: Some(KEY_SHA256.into()),
    ..NotetakerPolicy::default()
  };
  app.state.policy.set(policy);
}

async fn credentials(app: &TestApp, token: &str) -> (StatusCode, Value) {
  call_with_token(app, token, Method::GET, "/notetaker/credentials", None).await
}

mod key {
  use super::*;

  #[tokio::test]
  async fn admins_store_the_key_write_only_and_sealed() {
    let app = app!();
    let bob = admin();
    let initial = app.get(&bob, "/admin/notetaker").await;
    assert_eq!(initial.status, StatusCode::OK, "{}", initial.body);
    assert_eq!(initial.body["is_key_stored"], false);
    assert_eq!(initial.body["can_store_secrets"], true);

    let stored = app
      .put(
        &bob,
        "/admin/notetaker/key",
        json!({"key": {"action": "set", "value": format!(" {KEY}\n")}}),
      )
      .await;
    assert_eq!(stored.status, StatusCode::OK, "{}", stored.body);
    assert_eq!(stored.body["is_key_stored"], true);
    assert!(stored.body.get("key_updated_at").is_some());
    assert!(!stored.body.to_string().contains(KEY));
    let row: (String, Vec<u8>) = teamcraft_db::testing::sqlx::query_as(
      "SELECT value::text, sealed_secret FROM server_settings WHERE name = 'notetaker.openrouter'",
    )
    .fetch_one(app.sql_pool())
    .await
    .unwrap();
    assert!(!row.0.contains(KEY));
    assert!(!row.1.windows(KEY.len()).any(|window| window == KEY.as_bytes()));

    let kept = app
      .put(&bob, "/admin/notetaker/key", json!({"key": {"action": "keep"}}))
      .await;
    assert_eq!(kept.body["is_key_stored"], true);
    let cleared = app
      .put(&bob, "/admin/notetaker/key", json!({"key": {"action": "clear"}}))
      .await;
    assert_eq!(cleared.body["is_key_stored"], false);

    let audit = app.get(&bob, "/admin/audit?action=notetaker_key_set").await;
    assert_eq!(audit.body["entries"].as_array().unwrap().len(), 1);
    assert!(!audit.body.to_string().contains(KEY));
    app.stop().await;
  }

  #[tokio::test]
  async fn members_and_malformed_keys_are_refused() {
    let mut app = app!();
    let refused = app.get(&member("carol"), "/admin/notetaker").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let set = app
      .put(
        &member("carol"),
        "/admin/notetaker/key",
        json!({"key": {"action": "set", "value": KEY}}),
      )
      .await;
    assert_eq!(set.status, StatusCode::FORBIDDEN);
    for bad in ["", "sk or v1", &"x".repeat(600)] {
      let reply = app
        .put(
          &admin(),
          "/admin/notetaker/key",
          json!({"key": {"action": "set", "value": bad}}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    app.state.maybe_settings_key = None;
    app.reconfigure(|_| {});
    let without = app
      .put(
        &admin(),
        "/admin/notetaker/key",
        json!({"key": {"action": "set", "value": KEY}}),
      )
      .await;
    assert_eq!(without.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
      app.get(&admin(), "/admin/notetaker").await.body["can_store_secrets"],
      false
    );
    app.stop().await;
  }
}

mod credentials {
  use super::*;

  #[tokio::test]
  async fn only_the_bot_of_the_enabled_policy_gets_the_key() {
    let app = app!();
    let (bot, token) = create_bot(&app, "Notetaker").await;
    let (_, other_token) = create_bot(&app, "Log bridge").await;
    app
      .put(
        &admin(),
        "/admin/notetaker/key",
        json!({"key": {"action": "set", "value": KEY}}),
      )
      .await;

    // Off by default: nobody gets it.
    assert_eq!(credentials(&app, &token).await.0, StatusCode::FORBIDDEN);
    enable_notetaker(&app, bot);
    let (status, body) = credentials(&app, &token).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["api_key"], KEY);
    assert_eq!(credentials(&app, &other_token).await.0, StatusCode::FORBIDDEN);
    let refused = app.get(&member("carol"), "/notetaker/credentials").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let admin_reply = app.get(&admin(), "/notetaker/credentials").await;
    assert_eq!(admin_reply.status, StatusCode::FORBIDDEN);
    app.stop().await;
  }

  #[tokio::test]
  async fn without_a_stored_key_the_bot_hears_so() {
    let app = app!();
    let (bot, token) = create_bot(&app, "Notetaker").await;
    enable_notetaker(&app, bot);
    let (status, body) = credentials(&app, &token).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error_code"], "notetaker_key_not_found");
    app.stop().await;
  }
}

mod policy {
  use super::*;

  fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
  }

  fn payload(edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut value = serde_json::to_value(PolicyDocument {
      version: 1,
      policy: WorkspacePolicy::default(),
    })
    .unwrap();
    edit(&mut value["policy"]["notetaker"]);
    serde_json::to_vec(&value).unwrap()
  }

  #[tokio::test]
  async fn malformed_groups_and_non_bot_accounts_are_refused() {
    let app = app!();
    let owner = owner();
    let device = register_device(&app, &owner).await;
    let (bot, _) = create_bot(&app, "Notetaker").await;
    let carol = app.get(&member("carol"), "/me").await.body["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let append = |payload: Vec<u8>| {
      let app = &app;
      let owner = &owner;
      let device = device.clone();
      async move {
        app
          .post(
            owner,
            "/workspace/policy",
            json!({"expected_version": 0, "payload": b64(&payload), "signature": b64(b"sig"), "signer_device_id": device}),
          )
          .await
          .status
      }
    };
    let enabled = |notetaker: &mut Value, account: &str| {
      notetaker["is_enabled"] = json!(true);
      notetaker["bot"] = json!(account);
      notetaker["api_key_sha256"] = json!(KEY_SHA256);
    };
    let cases: Vec<Vec<u8>> = vec![
      payload(|n| n["is_enabled"] = json!(true)),
      payload(|n| n["transcription_model"] = json!("whisper")),
      payload(|n| n["inviters"] = json!("everyone")),
      payload(|n| n["language"] = json!("German")),
      payload(|n| enabled(n, &carol)),
    ];
    for case in cases {
      assert_eq!(append(case).await, StatusCode::BAD_REQUEST);
    }
    assert_eq!(app.get(&owner, "/workspace/policy").await.body["version"], 0);
    let bot_text = bot.to_string();
    assert_eq!(
      append(payload(|n| enabled(n, &bot_text))).await,
      StatusCode::CREATED
    );
    let policy = app.state.policy.get();
    assert!(policy.notetaker.is_notetaker(bot));
    app.stop().await;
  }
}
