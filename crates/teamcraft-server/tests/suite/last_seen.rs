//! "Last seen" (PLAN.md §4): recorded when a user stops being active, shown only to people who may
//! see it, hidden by the user's choice, by appearing offline and by the workspace switch.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

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

#[tokio::test]
async fn it_is_recorded_when_the_last_device_goes_and_shown_only_while_offline() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  let device = register_device(&app, &carol).await;

  assert!(
    presence_of(&app, &dave, &carol_id)
      .await
      .get("last_seen")
      .is_none()
  );

  let socket = ws::connect(&app, &carol, &device).await;
  assert!(
    presence_of(&app, &dave, &carol_id)
      .await
      .get("last_seen")
      .is_none()
  );
  drop(socket);

  let entry = wait_for_last_seen(&app, &dave, &carol_id).await;
  assert_eq!(entry["state"], "offline");
  let seen = entry["last_seen"].as_i64().unwrap();
  let now = time::OffsetDateTime::now_utc().unix_timestamp() * 1000;
  assert!((now - seen).abs() < 60_000, "{seen} vs {now}");
  app.stop().await;
}

#[tokio::test]
async fn people_can_hide_it_and_the_choice_is_stored() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  let device = register_device(&app, &carol).await;

  let initial = app.get(&carol, "/me/privacy").await;
  assert_eq!(
    initial.body,
    json!({"last_seen": "everyone", "is_last_seen_available": true})
  );

  drop(ws::connect(&app, &carol, &device).await);
  wait_for_last_seen(&app, &dave, &carol_id).await;

  let hidden = app
    .put(&carol, "/me/privacy", json!({"last_seen": "nobody"}))
    .await;
  assert_eq!(hidden.status, StatusCode::OK, "{}", hidden.body);
  assert_eq!(hidden.body["last_seen"], "nobody");
  assert!(
    presence_of(&app, &dave, &carol_id)
      .await
      .get("last_seen")
      .is_none()
  );

  app
    .put(&carol, "/me/privacy", json!({"last_seen": "everyone"}))
    .await;
  assert!(
    presence_of(&app, &dave, &carol_id)
      .await
      .get("last_seen")
      .is_some()
  );

  let refused = app
    .put(&carol, "/me/privacy", json!({"last_seen": "friends"}))
    .await;
  assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
  app.stop().await;
}

#[tokio::test]
async fn appearing_offline_hides_it() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  let device = register_device(&app, &carol).await;

  drop(ws::connect(&app, &carol, &device).await);
  wait_for_last_seen(&app, &dave, &carol_id).await;

  app
    .put(&carol, "/me/presence", json!({"manual": "invisible"}))
    .await;
  assert!(
    presence_of(&app, &dave, &carol_id)
      .await
      .get("last_seen")
      .is_none()
  );
  app.put(&carol, "/me/presence", json!({"manual": "auto"})).await;
  assert!(
    presence_of(&app, &dave, &carol_id)
      .await
      .get("last_seen")
      .is_some()
  );
  app.stop().await;
}

#[tokio::test]
async fn the_workspace_can_switch_it_off() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  let device = register_device(&app, &carol).await;

  drop(ws::connect(&app, &carol, &device).await);
  wait_for_last_seen(&app, &dave, &carol_id).await;

  let mut policy = app.state.policy.get().as_ref().clone();
  policy.presence.is_last_seen_enabled = false;
  app.state.policy.set(policy);

  assert!(
    presence_of(&app, &dave, &carol_id)
      .await
      .get("last_seen")
      .is_none()
  );
  let privacy = app.get(&carol, "/me/privacy").await;
  assert_eq!(privacy.body["is_last_seen_available"], false);
  app.stop().await;
}

#[tokio::test]
async fn guests_only_see_it_for_people_they_share_a_conversation_with() {
  let app = app!();
  let (carol, erin) = (member("carol"), guest("erin"));
  let carol_id = user_id(&app, &carol).await;
  let device = register_device(&app, &carol).await;

  drop(ws::connect(&app, &carol, &device).await);
  wait_for_last_seen(&app, &member("dave"), &carol_id).await;

  let listed = app.get(&erin, &format!("/presence?user_ids={carol_id}")).await;
  assert_eq!(listed.body["presences"], json!([]));
  app.stop().await;
}

async fn presence_of(app: &TestApp, viewer: &TestUser, user: &str) -> Value {
  let listed = app.get(viewer, &format!("/presence?user_ids={user}")).await;
  assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
  listed.body["presences"][0].clone()
}

/// The disconnect is processed in a background task: wait until it has been recorded.
async fn wait_for_last_seen(app: &TestApp, viewer: &TestUser, user: &str) -> Value {
  for _ in 0..100 {
    let entry = presence_of(app, viewer, user).await;
    if entry.get("last_seen").is_some() {
      return entry;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
  }
  panic!("no last_seen for {user}");
}
