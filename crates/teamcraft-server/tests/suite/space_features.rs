//! Moving channels between spaces and muting a whole space, against a real server and Postgres.
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

mod moving_channels {
  use super::*;

  #[tokio::test]
  async fn a_space_admin_moves_a_channel_once_its_members_belong_to_the_new_space() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let source = create_space(&app, &alice, "Project A", "invite_only").await;
    let target = create_space(&app, &alice, "Project B", "invite_only").await;
    let carol_id = user_id(&app, &carol).await;
    add_space_member(&app, &alice, &source, &carol_id, "member").await;
    let channel = create_channel(&app, &alice, &source, "general", "private").await;
    add_channel_member(&app, &alice, &channel, &carol_id).await;

    // Carol is not in the target space: the channel would escape her space membership.
    let path = format!("/conversations/{channel}/space");
    let refused = app.put(&alice, &path, json!({"space_id": target})).await;
    assert_eq!(refused.status, StatusCode::CONFLICT, "{}", refused.body);

    add_space_member(&app, &alice, &target, &carol_id, "member").await;
    let moved = app.put(&alice, &path, json!({"space_id": target})).await;
    assert_eq!(moved.status, StatusCode::OK, "{}", moved.body);
    assert_eq!(moved.body["space_id"], json!(target));

    // Carol keeps the channel.
    let seen = app.get(&carol, &format!("/conversations/{channel}")).await;
    assert_eq!(seen.status, StatusCode::OK, "{}", seen.body);
    assert_eq!(seen.body["space_id"], json!(target));

    let events = app
      .get(&carol, &format!("/conversations/{channel}/system-events"))
      .await;
    let kinds: Vec<_> = events.body["events"]
      .as_array()
      .unwrap()
      .iter()
      .map(|event| event["kind"].as_str().unwrap().to_owned())
      .collect();
    assert!(kinds.contains(&"moved".to_owned()), "{kinds:?}");
    let audit = app.get(&alice, "/admin/audit?action=channel_moved").await;
    assert_eq!(
      audit.body["entries"].as_array().unwrap().len(),
      1,
      "{}",
      audit.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn moving_needs_admin_rights_in_both_spaces() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let source = create_space(&app, &alice, "Project A", "invite_only").await;
    let target = create_space(&app, &alice, "Project B", "invite_only").await;
    let carol_id = user_id(&app, &carol).await;
    add_space_member(&app, &alice, &source, &carol_id, "member").await;
    add_space_member(&app, &alice, &target, &carol_id, "member").await;
    // Carol owns the channel but is no space admin.
    let channel = create_channel(&app, &carol, &source, "general", "private").await;
    let path = format!("/conversations/{channel}/space");

    let denied = app.put(&carol, &path, json!({"space_id": target})).await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN, "{}", denied.body);

    // An admin of the source space only is not enough either.
    let dave = member("dave");
    let dave_id = user_id(&app, &dave).await;
    add_space_member(&app, &alice, &source, &dave_id, "space_admin").await;
    let half = app.put(&dave, &path, json!({"space_id": target})).await;
    assert_ne!(half.status, StatusCode::OK, "{}", half.body);

    let same = app.put(&alice, &path, json!({"space_id": source})).await;
    assert_eq!(same.status, StatusCode::BAD_REQUEST, "{}", same.body);
    app.stop().await;
  }
}

mod space_guests {
  use super::*;

  #[tokio::test]
  async fn a_guest_of_the_space_cannot_be_promoted_in_its_channels() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let carol_id = user_id(&app, &carol).await;
    // Carol is a member of the instance but only a guest of this space.
    add_space_member(&app, &alice, &space, &carol_id, "guest").await;
    let channel = create_channel(&app, &alice, &space, "general", "private").await;
    add_channel_member(&app, &alice, &channel, &carol_id).await;

    let path = format!("/conversations/{channel}/members/{carol_id}");
    for role in ["admin", "moderator"] {
      let promoted = app.patch(&alice, &path, json!({"role": role})).await;
      assert_eq!(
        promoted.status,
        StatusCode::BAD_REQUEST,
        "{role}: {}",
        promoted.body
      );
    }
    app.stop().await;
  }
}

mod muting_spaces {
  use super::*;

  #[tokio::test]
  async fn channels_inherit_the_space_setting_until_configured_individually() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let space = create_space(&app, &alice, "Project A", "invite_only").await;
    let carol_id = user_id(&app, &carol).await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    let channel = create_channel(&app, &alice, &space, "general", "private").await;
    add_channel_member(&app, &alice, &channel, &carol_id).await;

    let path = format!("/spaces/{space}/notification-settings");
    assert_eq!(app.get(&carol, &path).await.body, json!({}));
    let put = app.put(&carol, &path, json!({"level": "none"})).await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.body);
    assert_eq!(app.get(&carol, &path).await.body, json!({"level": "none"}));
    let listed = app.get(&carol, "/spaces").await;
    assert_eq!(
      listed.body["spaces"][0]["my_notifications"],
      json!({"level": "none"})
    );

    let conversation = app.get(&carol, &format!("/conversations/{channel}")).await;
    assert_eq!(
      conversation.body["my_membership"]["is_notification_inherited"],
      true
    );

    // A channel setting of her own wins; clearing it returns to the space.
    let own = format!("/conversations/{channel}/notification-settings");
    app.put(&carol, &own, json!({"level": "all"})).await;
    let conversation = app.get(&carol, &format!("/conversations/{channel}")).await;
    assert_eq!(
      conversation.body["my_membership"]["is_notification_inherited"],
      false
    );
    app
      .put(&carol, &own, json!({"level": "all", "is_inherited": true}))
      .await;
    assert_eq!(app.get(&carol, &own).await.body["is_inherited"], true);

    // Outsiders have no setting for the space.
    let eve = member("eve");
    assert_eq!(app.get(&eve, &path).await.status, StatusCode::NOT_FOUND);
    let bad = app.put(&carol, &path, json!({"level": "loud"})).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    app.stop().await;
  }
}
