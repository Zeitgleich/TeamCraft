//! User groups: admin API and the mirror of the identity provider's groups.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::fake_keycloak::{CLIENT_SECRET, FakeKeycloak, fake_group, fake_user};
use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::GatewayEvent;
use teamcraft_server::services::oidc::sync_once;
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn handles(reply: &teamcraft_server::testing::Reply) -> Vec<String> {
  reply.body["groups"]
    .as_array()
    .unwrap()
    .iter()
    .map(|group| group["handle"].as_str().unwrap().to_owned())
    .collect()
}

mod management {
  use super::*;

  #[tokio::test]
  async fn admins_manage_groups_and_members_see_them() {
    let app = app!();
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);

    let denied = app
      .post(
        &carol,
        "/user-groups",
        json!({"handle": "devteam", "name": "Developers"}),
      )
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let created = app
      .post(
        &bob,
        "/user-groups",
        json!({"handle": "@DevTeam", "name": "Developers", "description": "All devs"}),
      )
      .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert_eq!(created.body["handle"], "devteam");
    assert_eq!(created.body["is_synced"], false);
    let group = created.body["id"].as_str().unwrap().to_owned();

    for user in [&carol_id, &dave_id] {
      let added = app
        .put(&bob, &format!("/user-groups/{group}/members/{user}"), json!({}))
        .await;
      assert_eq!(added.status, StatusCode::NO_CONTENT, "{}", added.body);
    }
    // Adding twice is harmless; members cannot manage; members can read.
    assert_eq!(
      app
        .put(
          &bob,
          &format!("/user-groups/{group}/members/{carol_id}"),
          json!({})
        )
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app
        .delete(&carol, &format!("/user-groups/{group}/members/{dave_id}"))
        .await
        .status,
      StatusCode::FORBIDDEN
    );
    let detail = app.get(&carol, &format!("/user-groups/{group}")).await;
    assert_eq!(detail.status, StatusCode::OK);
    assert_eq!(detail.body["group"]["member_count"], 2);
    assert_eq!(detail.body["members"].as_array().unwrap().len(), 2);
    assert_eq!(handles(&app.get(&carol, "/user-groups").await), vec!["devteam"]);

    assert_eq!(
      app
        .delete(&bob, &format!("/user-groups/{group}/members/{dave_id}"))
        .await
        .status,
      StatusCode::NO_CONTENT
    );
    let renamed = app
      .patch(
        &bob,
        &format!("/user-groups/{group}"),
        json!({"name": "Dev Team", "clears_description": true}),
      )
      .await;
    assert_eq!(renamed.status, StatusCode::OK, "{}", renamed.body);
    assert_eq!(renamed.body["name"], "Dev Team");
    assert!(renamed.body.get("description").is_none());
    assert_eq!(renamed.body["member_count"], 1);

    assert_eq!(
      app.delete(&bob, &format!("/user-groups/{group}")).await.status,
      StatusCode::NO_CONTENT
    );
    assert_eq!(
      app
        .get(&carol, &format!("/user-groups/{group}"))
        .await
        .error_code(),
      "user_group_not_found"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn handles_are_checked_and_unique_and_never_a_username() {
    let app = app!();
    let (bob, carol) = (admin(), member("carol"));
    user_id(&app, &carol).await;
    for handle in [
      "",
      "a",
      "Dev Team",
      "-dev",
      "all",
      "channel",
      "here",
      &"x".repeat(40),
    ] {
      let reply = app
        .post(&bob, "/user-groups", json!({"handle": handle, "name": "N"}))
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{handle:?}");
    }
    assert_eq!(
      app
        .post(&bob, "/user-groups", json!({"handle": "devs", "name": "N"}))
        .await
        .status,
      StatusCode::CREATED
    );
    assert_eq!(
      app
        .post(&bob, "/user-groups", json!({"handle": "devs", "name": "Other"}))
        .await
        .status,
      StatusCode::CONFLICT
    );
    assert_eq!(
      app
        .post(&bob, "/user-groups", json!({"handle": "carol", "name": "Carol"}))
        .await
        .status,
      StatusCode::CONFLICT,
      "a group must not shadow a person"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn guests_bots_and_deactivated_people_cannot_be_members_and_guests_cannot_read() {
    let app = app!();
    let (bob, ext) = (admin(), guest("ext"));
    let ext_id = user_id(&app, &ext).await;
    let group = app
      .post(&bob, "/user-groups", json!({"handle": "devs", "name": "Devs"}))
      .await
      .body["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let reply = app
      .put(&bob, &format!("/user-groups/{group}/members/{ext_id}"), json!({}))
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert_eq!(app.get(&ext, "/user-groups").await.status, StatusCode::FORBIDDEN);
    assert_eq!(
      app
        .put(&bob, &format!("/user-groups/{group}/members/1"), json!({}))
        .await
        .error_code(),
      "user_not_found"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn changes_reach_everybody_online() {
    let app = app!();
    let (bob, carol) = (admin(), member("carol"));
    let device = register_device(&app, &carol).await;
    let mut socket = ws::connect(&app, &carol, &device).await;
    ws::events(&mut socket, Duration::from_millis(300)).await;
    app
      .post(&bob, "/user-groups", json!({"handle": "devs", "name": "Devs"}))
      .await;
    let events = ws::events(&mut socket, Duration::from_millis(500)).await;
    assert!(
      events
        .iter()
        .any(|event| matches!(event, GatewayEvent::UserGroupUpdate { .. }))
    );
    app.stop().await;
  }
}

mod provider_groups {
  use super::*;

  async fn app_with_groups(sync_groups: bool) -> Option<(TestApp, FakeKeycloak)> {
    let provider = FakeKeycloak::start(vec![
      fake_user("sub-alice", &["owner"]),
      fake_user("sub-bob", &["admin"]),
      fake_user("sub-carol", &["member"]),
      fake_user("sub-dave", &["member"]),
    ])
    .await;
    let mut settings = provider.sync_settings(CLIENT_SECRET, 2);
    settings.sync_groups = sync_groups;
    let app = TestApp::start_with(|s| s.maybe_oidc_sync = Some(settings)).await?;
    Some((app, provider))
  }

  #[tokio::test]
  async fn groups_and_members_follow_the_provider() {
    let Some((app, provider)) = app_with_groups(true).await else {
      return;
    };
    let (bob, carol, dave) = (admin(), member("carol"), member("dave"));
    let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
    user_id(&app, &bob).await;
    provider.set_groups(vec![
      fake_group("g-1", "Dev Team", &["sub-carol", "sub-dave", "sub-unknown"]),
      fake_group("g-2", "!!!", &["sub-carol"]),
    ]);

    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(report.groups_created, 1, "the second name cannot become a handle");
    let listed = app.get(&carol, "/user-groups").await;
    assert_eq!(handles(&listed), vec!["dev-team"]);
    let group = listed.body["groups"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(listed.body["groups"][0]["is_synced"], true);
    assert_eq!(listed.body["groups"][0]["member_count"], 2);

    // Synced groups are not edited here.
    for reply in [
      app.delete(&bob, &format!("/user-groups/{group}")).await,
      app
        .put(
          &bob,
          &format!("/user-groups/{group}/members/{carol_id}"),
          json!({}),
        )
        .await,
      app
        .delete(&bob, &format!("/user-groups/{group}/members/{dave_id}"))
        .await,
      app
        .patch(&bob, &format!("/user-groups/{group}"), json!({"name": "x"}))
        .await,
    ] {
      assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.body);
    }

    // Nothing changed: nothing reported. Then a member leaves and the group is renamed.
    assert_eq!(sync_once(&app.state).await.unwrap().groups_changed, 0);
    provider.set_groups(vec![fake_group("g-1", "Developers", &["sub-carol"])]);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(
      (
        report.groups_changed,
        report.groups_created,
        report.groups_deleted
      ),
      (1, 0, 0)
    );
    let detail = app.get(&carol, &format!("/user-groups/{group}")).await;
    assert_eq!(detail.body["group"]["name"], "Developers");
    assert_eq!(
      detail.body["group"]["handle"], "dev-team",
      "mentions keep working"
    );
    assert_eq!(detail.body["members"].as_array().unwrap().len(), 1);

    // A group that vanished at the provider vanishes here.
    provider.set_groups(vec![]);
    assert_eq!(sync_once(&app.state).await.unwrap().groups_deleted, 1);
    assert_eq!(
      handles(&app.get(&carol, "/user-groups").await),
      Vec::<String>::new()
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_mirror_is_off_unless_enabled() {
    let Some((app, provider)) = app_with_groups(false).await else {
      return;
    };
    let carol = member("carol");
    user_id(&app, &carol).await;
    provider.set_groups(vec![fake_group("g-1", "Dev Team", &["sub-carol"])]);
    sync_once(&app.state).await.unwrap();
    let listed: Value = app.get(&carol, "/user-groups").await.body;
    assert_eq!(listed["groups"].as_array().unwrap().len(), 0);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_local_group_keeps_its_handle_against_the_provider() {
    let Some((app, provider)) = app_with_groups(true).await else {
      return;
    };
    let (bob, carol) = (admin(), member("carol"));
    user_id(&app, &carol).await;
    app
      .post(
        &bob,
        "/user-groups",
        json!({"handle": "dev-team", "name": "Mine"}),
      )
      .await;
    provider.set_groups(vec![fake_group("g-1", "Dev Team", &["sub-carol"])]);
    let report = sync_once(&app.state).await.unwrap();
    assert_eq!(
      report.groups_created, 0,
      "the handle is taken; the provider's group is skipped"
    );
    let listed = app.get(&carol, "/user-groups").await;
    assert_eq!(listed.body["groups"][0]["name"], "Mine");
    app.stop().await;
  }
}
