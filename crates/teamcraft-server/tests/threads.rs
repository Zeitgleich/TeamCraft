//! Followed threads, the threads inbox and unread counts against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::{Reply, TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

/// alice, bob and carol share a private channel; each has a device.
struct Scene {
  channel: String,
  alice: TestUser,
  bob: TestUser,
  carol: TestUser,
  alice_device: String,
  bob_device: String,
  carol_device: String,
  alice_id: String,
  carol_id: String,
}

async fn scene(app: &TestApp) -> Scene {
  let (alice, bob, carol) = (owner(), member("bob"), member("carol"));
  let channel = channel_with(app, &[&bob, &carol]).await;
  Scene {
    channel,
    alice_device: register_device(app, &alice).await,
    bob_device: register_device(app, &bob).await,
    carol_device: register_device(app, &carol).await,
    alice_id: user_id(app, &alice).await,
    carol_id: user_id(app, &carol).await,
    alice,
    bob,
    carol,
  }
}

mod following {
  use super::*;

  #[tokio::test]
  async fn replying_follows_and_the_root_author_follows_when_others_reply() {
    let app = app!();
    let s = scene(&app).await;
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;

    send_in(&app, &s.bob, &s.channel, &s.bob_device, 2, Some(&root), &[]).await;

    let bob_threads = threads(&app, &s.bob, "").await;
    assert_eq!(bob_threads.body["threads"][0]["thread_id"], root.as_str());
    assert_eq!(bob_threads.body["threads"][0]["follow_reason"], "replied");
    assert_eq!(bob_threads.body["threads"][0]["unread_count"], 0);
    let alice_threads = threads(&app, &s.alice, "").await;
    assert_eq!(alice_threads.body["threads"][0]["follow_reason"], "started");
    assert_eq!(alice_threads.body["threads"][0]["unread_count"], 1);
    assert_eq!(alice_threads.body["threads"][0]["reply_count"], 1);
    assert!(alice_threads.body["threads"][0]["last_reply"]["id"].is_string());
    assert!(
      threads(&app, &s.carol, "").await.body["threads"]
        .as_array()
        .unwrap()
        .is_empty()
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn mentioned_members_follow_the_thread() {
    let app = app!();
    let s = scene(&app).await;
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;

    send_in(
      &app,
      &s.bob,
      &s.channel,
      &s.bob_device,
      2,
      Some(&root),
      &[&s.carol_id],
    )
    .await;

    let carol_threads = threads(&app, &s.carol, "").await;
    assert_eq!(carol_threads.body["threads"][0]["follow_reason"], "mentioned");
    assert_eq!(carol_threads.body["threads"][0]["mention_count"], 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn unfollowing_sticks_until_the_user_replies_or_follows_again() {
    let app = app!();
    let s = scene(&app).await;
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    send_in(&app, &s.bob, &s.channel, &s.bob_device, 2, Some(&root), &[]).await;

    let unfollow = app
      .delete(
        &s.alice,
        &format!("/conversations/{}/threads/{root}/follow", s.channel),
      )
      .await;
    assert_eq!(unfollow.status, StatusCode::NO_CONTENT);
    send_in(
      &app,
      &s.bob,
      &s.channel,
      &s.bob_device,
      3,
      Some(&root),
      &[&s.alice_id],
    )
    .await;
    assert!(
      threads(&app, &s.alice, "").await.body["threads"]
        .as_array()
        .unwrap()
        .is_empty()
    );

    send_in(&app, &s.alice, &s.channel, &s.alice_device, 4, Some(&root), &[]).await;
    assert_eq!(
      threads(&app, &s.alice, "").await.body["threads"]
        .as_array()
        .unwrap()
        .len(),
      1
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn manual_follow_starts_from_now_and_only_works_on_thread_roots_for_members() {
    let app = app!();
    let s = scene(&app).await;
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    let reply = send_in(&app, &s.bob, &s.channel, &s.bob_device, 2, Some(&root), &[]).await;
    let path = |id: &str| format!("/conversations/{}/threads/{id}/follow", s.channel);

    assert_eq!(
      app.put(&s.carol, &path(&root), json!({})).await.status,
      StatusCode::NO_CONTENT
    );
    let carol = threads(&app, &s.carol, "").await;
    assert_eq!(carol.body["threads"][0]["follow_reason"], "manual");
    assert_eq!(
      carol.body["threads"][0]["unread_count"], 0,
      "existing replies count as read"
    );
    send_in(&app, &s.bob, &s.channel, &s.bob_device, 3, Some(&root), &[]).await;
    assert_eq!(
      threads(&app, &s.carol, "").await.body["threads"][0]["unread_count"],
      1
    );

    // A reply is not a thread root.
    assert_eq!(
      app.put(&s.carol, &path(&reply), json!({})).await.status,
      StatusCode::BAD_REQUEST
    );
    // Strangers can't follow.
    let eve = member("eve");
    assert_eq!(
      app.put(&eve, &path(&root), json!({})).await.status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }
}

mod unread {
  use super::*;

  #[tokio::test]
  async fn thread_read_positions_clear_the_unread_counts_and_filter_the_inbox() {
    let app = app!();
    let s = scene(&app).await;
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    send_in(&app, &s.bob, &s.channel, &s.bob_device, 2, Some(&root), &[]).await;
    let second = send_in(
      &app,
      &s.bob,
      &s.channel,
      &s.bob_device,
      3,
      Some(&root),
      &[&s.alice_id],
    )
    .await;

    let before = unread(&app, &s.alice).await;
    assert_eq!(
      (
        before.body["unread_thread_count"].as_u64(),
        before.body["thread_mention_count"].as_u64()
      ),
      (Some(1), Some(1))
    );
    assert_eq!(
      threads(&app, &s.alice, "?unread_only=true").await.body["threads"]
        .as_array()
        .unwrap()
        .len(),
      1
    );

    let read = app
      .put(
        &s.alice,
        &format!("/conversations/{}/threads/{root}/read", s.channel),
        json!({"message_id": second}),
      )
      .await;
    assert_eq!(read.status, StatusCode::NO_CONTENT, "{}", read.body);

    let after = unread(&app, &s.alice).await;
    assert_eq!(
      (
        after.body["unread_thread_count"].as_u64(),
        after.body["thread_mention_count"].as_u64()
      ),
      (Some(0), Some(0))
    );
    assert!(
      threads(&app, &s.alice, "?unread_only=true").await.body["threads"]
        .as_array()
        .unwrap()
        .is_empty()
    );
    assert_eq!(
      threads(&app, &s.alice, "").await.body["threads"]
        .as_array()
        .unwrap()
        .len(),
      1
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn reading_a_thread_that_is_not_followed_or_a_foreign_message_is_rejected() {
    let app = app!();
    let s = scene(&app).await;
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    let reply = send_in(&app, &s.bob, &s.channel, &s.bob_device, 2, Some(&root), &[]).await;
    let other = send(&app, &s.bob, &s.channel, &s.bob_device, 3).await;
    let path = format!("/conversations/{}/threads/{root}/read", s.channel);

    let not_followed = app.put(&s.carol, &path, json!({"message_id": reply})).await;
    let not_a_reply = app.put(&s.bob, &path, json!({"message_id": other})).await;

    assert_eq!(not_followed.status, StatusCode::CONFLICT, "{}", not_followed.body);
    assert_eq!(
      not_a_reply.status,
      StatusCode::BAD_REQUEST,
      "{}",
      not_a_reply.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn conversation_counts_use_the_read_position_and_ignore_own_messages_and_thread_replies() {
    let app = app!();
    let s = scene(&app).await;
    let first = send(&app, &s.bob, &s.channel, &s.bob_device, 1).await;
    send_in(&app, &s.bob, &s.channel, &s.bob_device, 2, None, &[&s.alice_id]).await;
    send(&app, &s.alice, &s.channel, &s.alice_device, 3).await;
    send_in(&app, &s.bob, &s.channel, &s.bob_device, 4, Some(&first), &[]).await;

    let counts = unread(&app, &s.alice).await;
    assert_eq!(
      counts.body["conversations"][0]["conversation_id"],
      s.channel.as_str()
    );
    assert_eq!(counts.body["conversations"][0]["unread_count"], 2);
    assert_eq!(counts.body["conversations"][0]["mention_count"], 1);

    app
      .put(
        &s.alice,
        &format!("/conversations/{}/read-position", s.channel),
        json!({"message_id": first}),
      )
      .await;
    let counts = unread(&app, &s.alice).await;
    assert_eq!(counts.body["conversations"][0]["unread_count"], 1);
    assert_eq!(counts.body["conversations"][0]["mention_count"], 1);

    // Nobody else's counts change.
    assert_eq!(
      unread(&app, &s.bob).await.body["conversations"][0]["unread_count"],
      1
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn messages_from_before_joining_do_not_count() {
    let app = app!();
    let (alice, bob) = (owner(), member("bob"));
    let alice_device = register_device(&app, &alice).await;
    let channel = channel_with(&app, &[]).await;
    send(&app, &alice, &channel, &alice_device, 1).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let bob_id = user_id(&app, &bob).await;
    add_channel_member_by_space(&app, &alice, &channel, &bob_id).await;

    assert!(
      unread(&app, &bob).await.body["conversations"]
        .as_array()
        .unwrap()
        .is_empty()
    );
    send(&app, &alice, &channel, &alice_device, 2).await;
    assert_eq!(
      unread(&app, &bob).await.body["conversations"][0]["unread_count"],
      1
    );
    app.stop().await;
  }
}

mod caps {
  use super::*;

  #[tokio::test]
  async fn unread_counts_stop_at_the_configured_limit() {
    let Some(app) = TestApp::start_with(|s| s.limits.max_unread_counted = 3).await else {
      return;
    };
    let s = scene(&app).await;
    for n in 1..=5 {
      let ids = if n == 5 { vec![s.alice_id.as_str()] } else { vec![] };
      send_in(&app, &s.bob, &s.channel, &s.bob_device, n, None, &ids).await;
    }

    let counts = unread(&app, &s.alice).await;

    // Five unread messages, but only the newest three are looked at; the mention is the newest.
    assert_eq!(counts.body["conversations"][0]["unread_count"], 3);
    assert_eq!(counts.body["conversations"][0]["mention_count"], 1);
    app.stop().await;
  }
}

mod gateway {
  use super::*;

  #[tokio::test]
  async fn follow_changes_reach_only_the_affected_users_devices() {
    let app = app!();
    let s = scene(&app).await;
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    let mut alice_socket = ws::connect(&app, &s.alice, &s.alice_device).await;
    let mut carol_socket = ws::connect(&app, &s.carol, &s.carol_device).await;
    ws::events(&mut alice_socket, Duration::from_millis(300)).await;
    ws::events(&mut carol_socket, Duration::from_millis(100)).await;

    send_in(&app, &s.bob, &s.channel, &s.bob_device, 2, Some(&root), &[]).await;

    let seen = thread_updates(&ws::events(&mut alice_socket, Duration::from_millis(500)).await);
    assert_eq!(seen, [(s.channel.clone(), root.clone())]);
    assert!(thread_updates(&ws::events(&mut carol_socket, Duration::from_millis(300)).await).is_empty());
    app.stop().await;
  }
}

async fn threads(app: &TestApp, user: &TestUser, query: &str) -> Reply {
  let reply = app.get(user, &format!("/threads{query}")).await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply
}

async fn unread(app: &TestApp, user: &TestUser) -> Reply {
  let reply = app.get(user, "/unread").await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply
}

fn thread_updates(events: &[teamcraft_proto::GatewayEvent]) -> Vec<(String, String)> {
  events
    .iter()
    .map(|event| serde_json::to_value(event).unwrap())
    .filter(|json: &Value| json["type"] == "thread_update")
    .map(|json| {
      (
        json["conversation_id"].as_str().unwrap().to_owned(),
        json["thread_id"].as_str().unwrap().to_owned(),
      )
    })
    .collect()
}

/// Adds a space member to the channel through the channel's space (alice owns both).
async fn add_channel_member_by_space(app: &TestApp, by: &TestUser, channel: &str, user: &str) {
  let space = app.get(by, &format!("/conversations/{channel}")).await.body["space_id"]
    .as_str()
    .unwrap()
    .to_owned();
  add_space_member(app, by, &space, user, "member").await;
  add_channel_member(app, by, channel, user).await;
}
