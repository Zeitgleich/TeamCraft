//! Bot accounts: admin registration, token authentication, rotation and revocation.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use reqwest::{Method, StatusCode};
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

async fn create_bot(app: &TestApp) -> (String, String) {
  let reply = app
    .post(
      &admin(),
      "/admin/bots",
      json!({"name": "Log bridge", "description": "Runs next to the NAS"}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  (
    reply.body["bot"]["id"].as_str().unwrap().to_owned(),
    reply.body["token"].as_str().unwrap().to_owned(),
  )
}

mod registration {
  use super::*;

  #[tokio::test]
  async fn admins_register_bots_and_the_token_works_like_an_access_token() {
    let app = app!();
    let (bot, token) = create_bot(&app).await;
    assert!(token.starts_with("tcb_"));

    let me = call_with_token(&app, &token, Method::GET, "/me", None).await;
    assert_eq!(me.0, StatusCode::OK, "{}", me.1);
    assert_eq!(
      (
        me.1["id"].as_str(),
        me.1["display_name"].as_str(),
        me.1["role"].as_str()
      ),
      (Some(bot.as_str()), Some("Log bridge"), Some("guest"))
    );
    let summary = app.get(&admin(), &format!("/users/{bot}")).await;
    assert_eq!(summary.body["kind"], "bot");

    let listed = app.get(&admin(), "/admin/bots").await;
    assert_eq!(listed.body["bots"][0]["id"], bot.as_str());
    assert_eq!(listed.body["bots"][0]["is_revoked"], false);
    assert!(
      !listed.body.to_string().contains(&token),
      "the token is shown once"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn only_admins_manage_bots() {
    let app = app!();
    let carol = member("carol");
    let (bot, _) = create_bot(&app).await;

    assert_eq!(
      app.post(&carol, "/admin/bots", json!({"name": "x"})).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(app.get(&carol, "/admin/bots").await.status, StatusCode::FORBIDDEN);
    assert_eq!(
      app.delete(&carol, &format!("/admin/bots/{bot}")).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .post(&carol, &format!("/admin/bots/{bot}/token"), json!({}))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(
      app
        .post(&admin(), "/admin/bots", json!({"name": "  "}))
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn unknown_and_malformed_tokens_are_refused() {
    let app = app!();
    create_bot(&app).await;
    for token in [
      "tcb_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
      "tcb_",
      "tcb_x.y.z",
    ] {
      assert_eq!(
        call_with_token(&app, token, Method::GET, "/me", None).await.0,
        StatusCode::UNAUTHORIZED,
        "{token}"
      );
    }
    app.stop().await;
  }
}

mod lifecycle {
  use super::*;

  #[tokio::test]
  async fn rotating_replaces_the_token_and_revoking_ends_the_account() {
    let app = app!();
    let (bot, old_token) = create_bot(&app).await;

    let rotated = app
      .post(&admin(), &format!("/admin/bots/{bot}/token"), json!({}))
      .await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    let new_token = rotated.body["token"].as_str().unwrap().to_owned();
    assert_ne!(new_token, old_token);
    assert_eq!(
      call_with_token(&app, &old_token, Method::GET, "/me", None)
        .await
        .0,
      StatusCode::UNAUTHORIZED
    );
    assert_eq!(
      call_with_token(&app, &new_token, Method::GET, "/me", None)
        .await
        .0,
      StatusCode::OK
    );

    assert_eq!(
      app.delete(&admin(), &format!("/admin/bots/{bot}")).await.status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      call_with_token(&app, &new_token, Method::GET, "/me", None)
        .await
        .0,
      StatusCode::UNAUTHORIZED
    );
    assert_eq!(
      app.get(&admin(), "/admin/bots").await.body["bots"][0]["is_revoked"],
      true
    );
    assert_eq!(
      app.delete(&admin(), &format!("/admin/bots/{bot}")).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      app
        .post(&admin(), &format!("/admin/bots/{bot}/token"), json!({}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    let audit = app.get(&admin(), "/admin/audit?action=bot_revoked").await;
    assert_eq!(
      audit.body["entries"].as_array().unwrap().len(),
      1,
      "{}",
      audit.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_bot_has_its_own_device_and_posts_into_channels_it_was_added_to() {
    let app = app!();
    let alice = owner();
    let (bot, token) = create_bot(&app).await;
    let space = create_space(&app, &alice, "Ops", "invite_only").await;
    let channel = create_channel(&app, &alice, &space, "logs", "private").await;
    add_space_member(&app, &alice, &space, &bot, "guest").await;
    add_channel_member(&app, &alice, &channel, &bot).await;

    let (status, device) = call_with_token(
      &app,
      &token,
      Method::POST,
      "/devices",
      Some(json!({"name": "bridge", "public_key": "cHVibGlj", "cross_signature": "c2ln"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{device}");
    let (status, message) = call_with_token(
      &app,
      &token,
      Method::POST,
      &format!("/conversations/{channel}/messages"),
      Some(message_body(1, device["id"].as_str().unwrap())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{message}");
    let history = app
      .get(&alice, &format!("/conversations/{channel}/messages"))
      .await;
    assert_eq!(history.body["messages"][0]["sender_user_id"], bot.as_str());

    // Anywhere else the bot sees nothing.
    let other = create_channel(&app, &alice, &space, "private", "private").await;
    let hidden = call_with_token(
      &app,
      &token,
      Method::GET,
      &format!("/conversations/{other}"),
      None,
    )
    .await;
    assert_eq!(hidden.0, StatusCode::NOT_FOUND);
    app.stop().await;
  }
}
