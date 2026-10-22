//! Message storage, validation, history and its limits against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

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

/// alice owns a space with a public channel "general" that carol also joined.
struct Scene {
  space: String,
  channel: String,
  alice: TestUser,
  carol: TestUser,
  alice_device: String,
  carol_device: String,
  carol_id: String,
}

async fn scene(app: &TestApp, history_policy: &str) -> Scene {
  let (alice, carol) = (owner(), member("carol"));
  let (alice_device, carol_device) = (
    register_device(app, &alice).await,
    register_device(app, &carol).await,
  );
  let carol_id = user_id(app, &carol).await;
  let space = create_space(app, &alice, "Team", "open").await;
  app
    .post(&carol, &format!("/spaces/{space}/join"), json!({}))
    .await;
  let channel = app
    .post(
      &alice,
      &format!("/spaces/{space}/channels"),
      json!({"name": "general", "visibility": "public", "history_policy": history_policy}),
    )
    .await
    .id();
  app
    .post(&carol, &format!("/conversations/{channel}/join"), json!({}))
    .await;
  Scene {
    space,
    channel,
    alice,
    carol,
    alice_device,
    carol_device,
    carol_id,
  }
}

/// Lets the clock tick so messages and memberships get distinct millisecond timestamps.
async fn tick() {
  tokio::time::sleep(Duration::from_millis(20)).await;
}

fn ids(reply: &teamcraft_server::testing::Reply) -> Vec<String> {
  reply.body["messages"]
    .as_array()
    .unwrap()
    .iter()
    .map(|m| m["id"].as_str().unwrap().to_owned())
    .collect()
}

mod sending {
  use super::*;

  #[tokio::test]
  async fn the_same_client_id_returns_the_stored_message() {
    let app = app!();
    let s = scene(&app, "from_join").await;
    let path = format!("/conversations/{}/messages", s.channel);
    let first = app.post(&s.carol, &path, message_body(1, &s.carol_device)).await;
    let again = app.post(&s.carol, &path, message_body(1, &s.carol_device)).await;
    assert_eq!(
      (first.status, again.status),
      (StatusCode::CREATED, StatusCode::OK)
    );
    assert_eq!(first.id(), again.id());
    assert_eq!(
      app.get(&s.alice, &path).await.body["messages"]
        .as_array()
        .unwrap()
        .len(),
      1
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn ciphertext_is_stored_verbatim_with_its_routing_metadata() {
    let app = app!();
    let s = scene(&app, "from_join").await;
    let path = format!("/conversations/{}/messages", s.channel);
    let mut body = message_body(1, &s.alice_device);
    body["mentioned_user_ids"] = json!([s.carol_id]);
    body["is_urgent"] = json!(true);
    let sent = app.post(&s.alice, &path, body).await;
    assert_eq!(sent.status, StatusCode::CREATED, "{}", sent.body);

    let listed = app.get(&s.carol, &path).await;
    let message = &listed.body["messages"][0];
    assert_eq!(message["ciphertext"], "Y2lwaGVydGV4dA==");
    assert_eq!(message["content_key_id"], "a2V5");
    assert_eq!(message["mentioned_user_ids"], json!([s.carol_id]));
    assert_eq!(message["is_urgent"], true);
    assert_eq!(message["sender_device_id"], s.alice_device);
    assert_eq!(message["client_message_id"], client_id(1));
    app.stop().await;
  }

  #[tokio::test]
  async fn threads_need_an_existing_root_message_of_the_conversation() {
    let app = app!();
    let s = scene(&app, "from_join").await;
    let path = format!("/conversations/{}/messages", s.channel);
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;

    let mut reply = message_body(2, &s.alice_device);
    reply["thread_id"] = json!(root);
    let ok = app.post(&s.alice, &path, reply).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.body);
    assert_eq!(ok.body["thread_id"], root);

    let mut dangling = message_body(3, &s.alice_device);
    dangling["thread_id"] = json!("12345");
    assert_eq!(
      app.post(&s.alice, &path, dangling).await.status,
      StatusCode::BAD_REQUEST
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_messages_are_rejected() {
    let Some(app) = TestApp::start_with(|settings| settings.limits.max_ciphertext_bytes = 12).await else {
      return;
    };
    let s = scene(&app, "from_join").await;
    let path = format!("/conversations/{}/messages", s.channel);

    let mut empty = message_body(1, &s.alice_device);
    empty["ciphertext"] = json!("");
    assert_eq!(
      app.post(&s.alice, &path, empty).await.status,
      StatusCode::BAD_REQUEST
    );

    let mut large = message_body(2, &s.alice_device);
    large["ciphertext"] = json!("AAAAAAAAAAAAAAAAAAAAAAAA");
    let too_large = app.post(&s.alice, &path, large).await;
    assert_eq!(too_large.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(too_large.error_code(), "payload_too_large");

    let mut stranger = message_body(3, &s.alice_device);
    stranger["mentioned_user_ids"] = json!(["424242"]);
    assert_eq!(
      app.post(&s.alice, &path, stranger).await.status,
      StatusCode::BAD_REQUEST
    );

    let bad_id = json!({"client_message_id": "xyz", "device_id": s.alice_device, "content_key_id": "a2V5", "ciphertext": "AA=="});
    assert_eq!(
      app.post(&s.alice, &path, bad_id).await.status,
      StatusCode::BAD_REQUEST
    );

    // A device of somebody else can't be used.
    let foreign = app.post(&s.alice, &path, message_body(4, &s.carol_device)).await;
    assert_eq!(foreign.error_code(), "device_not_found");
    app.stop().await;
  }

  #[tokio::test]
  async fn only_members_can_post_or_read() {
    let app = app!();
    let s = scene(&app, "from_join").await;
    let dave = member("dave");
    let dave_device = register_device(&app, &dave).await;
    app
      .post(&dave, &format!("/spaces/{}/join", s.space), json!({}))
      .await;
    let path = format!("/conversations/{}/messages", s.channel);

    // Dave sees the public channel but isn't in it.
    assert_eq!(
      app.post(&dave, &path, message_body(1, &dave_device)).await.status,
      StatusCode::FORBIDDEN
    );
    assert_eq!(app.get(&dave, &path).await.status, StatusCode::FORBIDDEN);
    // Outsiders of the space don't even learn that the channel exists.
    let outsider = guest("ext");
    assert_eq!(app.get(&outsider, &path).await.status, StatusCode::NOT_FOUND);
    app.stop().await;
  }
}

mod history {
  use super::*;

  #[tokio::test]
  async fn pages_walk_back_in_time_and_forward_from_a_position() {
    let app = app!();
    let s = scene(&app, "from_join").await;
    let path = format!("/conversations/{}/messages", s.channel);
    let mut sent = Vec::new();
    for n in 1..=5 {
      sent.push(send(&app, &s.carol, &s.channel, &s.carol_device, n).await);
    }

    let newest = app.get(&s.carol, &format!("{path}?limit=2")).await;
    assert_eq!(ids(&newest), vec![sent[4].clone(), sent[3].clone()]);
    assert_eq!(newest.body["has_more"], true);
    let older = app
      .get(&s.carol, &format!("{path}?limit=2&before={}", sent[3]))
      .await;
    assert_eq!(ids(&older), vec![sent[2].clone(), sent[1].clone()]);
    let oldest = app
      .get(&s.carol, &format!("{path}?limit=2&before={}", sent[1]))
      .await;
    assert_eq!(ids(&oldest), vec![sent[0].clone()]);
    assert_eq!(oldest.body["has_more"], false);

    let forward = app
      .get(&s.carol, &format!("{path}?limit=2&after={}", sent[1]))
      .await;
    assert_eq!(ids(&forward), vec![sent[2].clone(), sent[3].clone()]);
    assert_eq!(forward.body["has_more"], true);
    let both = app
      .get(&s.carol, &format!("{path}?before={}&after={}", sent[3], sent[1]))
      .await;
    assert_eq!(both.status, StatusCode::BAD_REQUEST);
    app.stop().await;
  }

  #[tokio::test]
  async fn joiners_only_see_history_after_joining_unless_the_channel_shares_it() {
    let app = app!();
    for (policy, sees_old) in [("from_join", false), ("full", true)] {
      let s = scene(&app, policy).await;
      let old = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
      tick().await;

      let dave = member("dave");
      app
        .post(&dave, &format!("/spaces/{}/join", s.space), json!({}))
        .await;
      app
        .post(&dave, &format!("/conversations/{}/join", s.channel), json!({}))
        .await;
      tick().await;
      let new = send(&app, &s.alice, &s.channel, &s.alice_device, 2).await;

      let seen = ids(
        &app
          .get(&dave, &format!("/conversations/{}/messages", s.channel))
          .await,
      );
      if sees_old {
        assert_eq!(seen, vec![new, old], "policy {policy}");
      } else {
        assert_eq!(seen, vec![new], "policy {policy}");
      }
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn guests_never_see_history_from_before_they_joined() {
    let app = app!();
    let s = scene(&app, "full").await;
    send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    tick().await;

    let ext = guest("ext");
    let ext_id = user_id(&app, &ext).await;
    add_space_member(&app, &s.alice, &s.space, &ext_id, "guest").await;
    add_channel_member(&app, &s.alice, &s.channel, &ext_id).await;
    tick().await;
    let fresh = send(&app, &s.alice, &s.channel, &s.alice_device, 2).await;

    let seen = ids(
      &app
        .get(&ext, &format!("/conversations/{}/messages", s.channel))
        .await,
    );
    assert_eq!(seen, vec![fresh]);
    app.stop().await;
  }

  #[tokio::test]
  async fn page_size_is_clamped() {
    let Some(app) = TestApp::start_with(|settings| settings.limits.max_page_size = 2).await else {
      return;
    };
    let s = scene(&app, "from_join").await;
    for n in 1..=3 {
      send(&app, &s.alice, &s.channel, &s.alice_device, n).await;
    }
    let page = app
      .get(
        &s.alice,
        &format!("/conversations/{}/messages?limit=500", s.channel),
      )
      .await;
    assert_eq!(page.body["messages"].as_array().unwrap().len(), 2);
    assert_eq!(page.body["has_more"], true);
    let zero: Value = app
      .get(
        &s.alice,
        &format!("/conversations/{}/messages?limit=0", s.channel),
      )
      .await
      .body;
    assert_eq!(zero["messages"].as_array().unwrap().len(), 1);
    app.stop().await;
  }
}
