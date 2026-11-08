//! `use_group_mentions` on a server that can't read messages (docs/e2ee.md §4.4): a sender without
//! it may name only a few people per message, and the groups' rosters stay with non-guests.
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

/// The default limit for senders without the permission (`[limits]`).
const LIMIT: usize = 10;

/// alice owns a space with channel "news"; ext is a guest, carol a member, bob an admin.
struct Scene {
  channel: String,
  alice: TestUser,
  bob: TestUser,
  carol: TestUser,
  ext: TestUser,
}

async fn scene(app: &TestApp) -> Scene {
  let (alice, bob, carol, ext) = (owner(), admin(), member("carol"), guest("ext"));
  let (carol_id, ext_id, bob_id) = (
    user_id(app, &carol).await,
    user_id(app, &ext).await,
    user_id(app, &bob).await,
  );
  let space = create_space(app, &alice, "Team", "open").await;
  add_space_member(app, &alice, &space, &carol_id, "member").await;
  add_space_member(app, &alice, &space, &bob_id, "member").await;
  add_space_member(app, &alice, &space, &ext_id, "guest").await;
  let channel = create_channel(app, &alice, &space, "news", "public").await;
  for id in [&carol_id, &ext_id, &bob_id] {
    add_channel_member(app, &alice, &channel, id).await;
  }
  Scene {
    channel,
    alice,
    bob,
    carol,
    ext,
  }
}

/// A message naming `count` IDs that aren't in the channel: past the permission check it fails as
/// an invalid request, so the status tells which check refused it.
async fn send_with_ids(app: &TestApp, channel: &str, user: &TestUser, count: usize) -> StatusCode {
  let device = register_device(app, user).await;
  let mut body = message_body(1, &device);
  let ids: Vec<Value> = (0..count)
    .map(|index| json!(format!("{}", 1_000 + index)))
    .collect();
  body["mentioned_user_ids"] = json!(ids);
  app
    .post(user, &format!("/conversations/{channel}/messages"), body)
    .await
    .status
}

#[tokio::test]
async fn every_role_gets_the_limit_its_permission_gives() {
  let app = app!();
  let s = scene(&app).await;
  // With the permission (owner, admin, member) only the membership of the IDs is left to refuse.
  for user in [&s.alice, &s.bob, &s.carol] {
    assert_eq!(
      send_with_ids(&app, &s.channel, user, LIMIT + 1).await,
      StatusCode::BAD_REQUEST,
      "past the permission check"
    );
  }
  // Guests have no `use_group_mentions` by default.
  assert_eq!(
    send_with_ids(&app, &s.channel, &s.ext, LIMIT + 1).await,
    StatusCode::FORBIDDEN
  );
  assert_eq!(
    send_with_ids(&app, &s.channel, &s.ext, LIMIT).await,
    StatusCode::BAD_REQUEST,
    "up to the limit a guest is treated like everybody"
  );
  app.stop().await;
}

#[tokio::test]
async fn channel_moderation_takes_the_permission_away_from_members() {
  let app = app!();
  let s = scene(&app).await;
  let path = format!("/admin/conversations/{}/moderation", s.channel);
  let reply = app
    .put(
      &s.alice,
      &path,
      json!({"moderation": {"member_denied": ["use_group_mentions"]}}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  assert_eq!(
    send_with_ids(&app, &s.channel, &s.carol, LIMIT + 1).await,
    StatusCode::FORBIDDEN
  );
  // An admin keeps the permission.
  assert_eq!(
    send_with_ids(&app, &s.channel, &s.bob, LIMIT + 1).await,
    StatusCode::BAD_REQUEST
  );
  app.stop().await;
}

#[tokio::test]
async fn rosters_are_for_non_guests_and_list_only_active_members() {
  let app = app!();
  let (alice, carol, dave, ext) = (admin(), member("carol"), member("dave"), guest("ext"));
  let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
  let group = app
    .post(
      &alice,
      "/user-groups",
      json!({"handle": "accounting", "name": "Accounting"}),
    )
    .await
    .id();
  for id in [&carol_id, &dave_id] {
    let added = app
      .put(&alice, &format!("/user-groups/{group}/members/{id}"), json!({}))
      .await;
    assert!(added.status.is_success(), "{}", added.body);
  }

  let rosters = app.get(&carol, "/user-groups/rosters").await;
  assert_eq!(rosters.status, StatusCode::OK, "{}", rosters.body);
  let roster = &rosters.body["groups"][0];
  assert_eq!(roster["handle"], "accounting");
  let mut listed: Vec<String> = roster["member_ids"]
    .as_array()
    .unwrap()
    .iter()
    .map(|id| id.as_str().unwrap().to_owned())
    .collect();
  listed.sort();
  let mut expected = vec![carol_id.clone(), dave_id.clone()];
  expected.sort();
  assert_eq!(listed, expected);

  assert_eq!(
    app.get(&ext, "/user-groups/rosters").await.status,
    StatusCode::FORBIDDEN,
    "guests read no groups"
  );

  // A deactivated account leaves the roster at once.
  let deactivated = app
    .post(&alice, &format!("/admin/users/{dave_id}/deactivate"), json!({}))
    .await;
  assert!(deactivated.status.is_success(), "{}", deactivated.body);
  let after = app.get(&carol, "/user-groups/rosters").await;
  assert_eq!(after.body["groups"][0]["member_ids"], json!([carol_id]));
  app.stop().await;
}
