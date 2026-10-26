//! `GET /sync/heads` (issue #714): the newest message and MLS entry per conversation and the member
//! list digests, against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

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

#[tokio::test]
async fn heads_follow_messages_mls_entries_and_members() {
  let app = app!();
  let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
  let channel = channel_with(&app, &[&carol, &dave]).await;
  let carol_device = register_device(&app, &carol).await;
  register_device(&app, &dave).await;

  let empty = head_of(&app, &carol, &channel).await;
  assert_eq!(empty.get("last_message_id"), None);
  assert_eq!(empty.get("last_mls_message_id"), None);
  // The same list for everyone in it: the same digest.
  assert_eq!(
    head_of(&app, &dave, &channel).await["members_digest"],
    empty["members_digest"]
  );

  let message = send(&app, &carol, &channel, &carol_device, 1).await;
  let reply = app
    .post(
      &carol,
      &format!("/conversations/{channel}/mls/commits"),
      json!({"device_id": carol_device, "epoch": 0, "commit": "Y29tbWl0", "welcomes": []}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  let backlog = app.get(&carol, &format!("/conversations/{channel}/mls")).await;
  let newest_mls = backlog.body["messages"].as_array().unwrap().last().unwrap()["id"].clone();
  let after = head_of(&app, &carol, &channel).await;
  assert_eq!(after["last_message_id"], json!(message));
  assert_eq!(after["last_mls_message_id"], newest_mls);
  assert_eq!(after["members_digest"], empty["members_digest"]);

  let erin_id = user_id(&app, &erin).await;
  let space = space_of(&app, &channel).await;
  add_space_member(&app, &owner(), &space, &erin_id, "member").await;
  add_channel_member(&app, &owner(), &channel, &erin_id).await;
  assert_ne!(
    head_of(&app, &carol, &channel).await["members_digest"],
    empty["members_digest"]
  );
  app.stop().await;
}

#[tokio::test]
async fn only_the_callers_conversations_and_no_space_lists_for_guests() {
  let app = app!();
  let (carol, bob, gina) = (member("carol"), member("bob"), guest("gina"));
  let channel = channel_with(&app, &[&carol]).await;
  let space = space_of(&app, &channel).await;
  let gina_id = user_id(&app, &gina).await;
  add_space_member(&app, &owner(), &space, &gina_id, "guest").await;
  add_channel_member(&app, &owner(), &channel, &gina_id).await;

  let outsider = heads(&app, &bob).await;
  assert_eq!(outsider["conversations"], json!([]));
  assert_eq!(outsider["spaces"], json!([]));

  let member_view = heads(&app, &carol).await;
  assert_eq!(member_view["spaces"][0]["space_id"], json!(space));
  let before = member_view["spaces"][0]["members_digest"].clone();

  let guest_view = heads(&app, &gina).await;
  assert_eq!(guest_view["conversations"][0]["conversation_id"], json!(channel));
  assert_eq!(guest_view["spaces"], json!([]));

  let bob_id = user_id(&app, &bob).await;
  add_space_member(&app, &owner(), &space, &bob_id, "member").await;
  assert_ne!(heads(&app, &carol).await["spaces"][0]["members_digest"], before);
  app.stop().await;
}

async fn heads(app: &TestApp, user: &TestUser) -> Value {
  let reply = app.get(user, "/sync/heads").await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body
}

async fn head_of(app: &TestApp, user: &TestUser, conversation: &str) -> Value {
  heads(app, user).await["conversations"]
    .as_array()
    .unwrap()
    .iter()
    .find(|head| head["conversation_id"] == json!(conversation))
    .cloned()
    .unwrap_or_else(|| panic!("no head for {conversation}"))
}

async fn space_of(app: &TestApp, conversation: &str) -> String {
  let reply = app.get(&owner(), &format!("/conversations/{conversation}")).await;
  reply.body["space_id"].as_str().unwrap().to_owned()
}
