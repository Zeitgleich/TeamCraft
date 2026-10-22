//! Custom status against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::GatewayEvent;
use teamcraft_server::services::presence::expire_statuses;
use teamcraft_server::testing::{Reply, TestApp};
use time::OffsetDateTime;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

const FAR_FUTURE: i64 = 4_102_444_800_000;

/// (user, state, status text) of every presence update.
fn updates(events: &[GatewayEvent]) -> Vec<(String, String, Option<String>)> {
  events
    .iter()
    .filter_map(|event| match event {
      GatewayEvent::PresenceUpdate {
        user_id,
        state,
        maybe_status,
      } => Some((
        user_id.to_string(),
        state.to_string(),
        maybe_status.as_ref().and_then(|status| status.maybe_text.clone()),
      )),
      _ => None,
    })
    .collect()
}

fn status_of(reply: &Reply, index: usize) -> serde_json::Value {
  reply.body["presences"][index]["status"].clone()
}

#[tokio::test]
async fn a_status_is_stored_returned_and_shown_in_the_presence_list_even_when_offline() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;

  let set = app
    .put(
      &carol,
      "/me/status",
      json!({"emoji": "🌴", "text": "Vacation until Monday", "expires_at": FAR_FUTURE}),
    )
    .await;
  assert_eq!(set.status, StatusCode::OK, "{}", set.body);
  assert_eq!(
    set.body,
    json!({"emoji": "🌴", "text": "Vacation until Monday", "expires_at": FAR_FUTURE})
  );

  let listed = app.get(&dave, &format!("/presence?user_ids={carol_id}")).await;
  assert_eq!(listed.body["presences"][0]["state"], "offline");
  assert_eq!(status_of(&listed, 0)["text"], "Vacation until Monday");

  // Replacing it keeps only the new one; clearing removes it.
  app.put(&carol, "/me/status", json!({"text": "Back"})).await;
  assert_eq!(
    status_of(
      &app.get(&dave, &format!("/presence?user_ids={carol_id}")).await,
      0
    ),
    json!({"text": "Back"})
  );
  assert_eq!(
    app.delete(&carol, "/me/status").await.status,
    StatusCode::NO_CONTENT
  );
  assert!(
    status_of(
      &app.get(&dave, &format!("/presence?user_ids={carol_id}")).await,
      0
    )
    .is_null()
  );
  app.stop().await;
}

#[tokio::test]
async fn invalid_statuses_are_rejected() {
  let app = app!();
  let carol = member("carol");
  for body in [
    json!({}),
    json!({"emoji": "", "text": "  "}),
    json!({"text": "x".repeat(500)}),
    json!({"emoji": "a\nb"}),
    json!({"emoji": "e".repeat(100)}),
    json!({"text": "ok", "expires_at": 1000}),
    json!({"text": "ok", "expires_at": i64::MAX}),
  ] {
    assert_eq!(
      app.put(&carol, "/me/status", body.clone()).await.status,
      StatusCode::BAD_REQUEST,
      "{body}"
    );
  }
  app.stop().await;
}

#[tokio::test]
async fn changes_reach_people_sharing_a_conversation_and_nobody_else() {
  let app = app!();
  let (carol, dave, erin) = (member("carol"), member("dave"), member("erin"));
  let carol_id = user_id(&app, &carol).await;
  channel_with(&app, &[&carol, &dave]).await;
  let (carol_device, dave_device, erin_device) = (
    register_device(&app, &carol).await,
    register_device(&app, &dave).await,
    register_device(&app, &erin).await,
  );
  let mut carol_socket = ws::connect(&app, &carol, &carol_device).await;
  let mut dave_socket = ws::connect(&app, &dave, &dave_device).await;
  let mut erin_socket = ws::connect(&app, &erin, &erin_device).await;
  // Let the connection presence updates pass.
  ws::events(&mut dave_socket, Duration::from_millis(400)).await;
  ws::events(&mut erin_socket, Duration::from_millis(100)).await;
  ws::events(&mut carol_socket, Duration::from_millis(100)).await;

  app.put(&carol, "/me/status", json!({"text": "Lunch"})).await;
  let seen = updates(&ws::events(&mut dave_socket, Duration::from_millis(500)).await);
  assert_eq!(
    seen,
    vec![(carol_id.clone(), "online".to_owned(), Some("Lunch".to_owned()))]
  );
  assert_eq!(
    updates(&ws::events(&mut carol_socket, Duration::from_millis(300)).await).len(),
    1,
    "her other devices too"
  );
  assert!(updates(&ws::events(&mut erin_socket, Duration::from_millis(300)).await).is_empty());

  app.delete(&carol, "/me/status").await;
  let seen = updates(&ws::events(&mut dave_socket, Duration::from_millis(500)).await);
  assert_eq!(seen, vec![(carol_id, "online".to_owned(), None)]);
  app.stop().await;
}

#[tokio::test]
async fn expired_statuses_disappear_and_are_announced() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let carol_id = user_id(&app, &carol).await;
  channel_with(&app, &[&carol, &dave]).await;
  let (carol_device, dave_device) = (
    register_device(&app, &carol).await,
    register_device(&app, &dave).await,
  );
  let _carol_socket = ws::connect(&app, &carol, &carol_device).await;
  let mut dave_socket = ws::connect(&app, &dave, &dave_device).await;
  let in_one_hour = (OffsetDateTime::now_utc().unix_timestamp() + 3600) * 1000;
  app
    .put(
      &carol,
      "/me/status",
      json!({"text": "In a meeting", "expires_at": in_one_hour}),
    )
    .await;
  ws::events(&mut dave_socket, Duration::from_millis(500)).await;

  // Nothing happens before the expiry time ...
  assert_eq!(
    expire_statuses(&app.state, OffsetDateTime::now_utc())
      .await
      .unwrap(),
    0
  );
  // ... and after it the status is gone from the list and from other people's view.
  let later = OffsetDateTime::now_utc() + time::Duration::hours(2);
  assert_eq!(expire_statuses(&app.state, later).await.unwrap(), 1);
  let seen = updates(&ws::events(&mut dave_socket, Duration::from_millis(500)).await);
  assert_eq!(seen, vec![(carol_id.clone(), "online".to_owned(), None)]);
  assert!(
    status_of(
      &app.get(&dave, &format!("/presence?user_ids={carol_id}")).await,
      0
    )
    .is_null()
  );
  app.stop().await;
}

#[tokio::test]
async fn guests_only_see_statuses_of_people_they_share_a_conversation_with() {
  let app = app!();
  let (carol, dave) = (member("carol"), member("dave"));
  let ext = guest("ext");
  let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
  user_id(&app, &ext).await;
  app.put(&carol, "/me/status", json!({"text": "Hello"})).await;
  app.put(&dave, "/me/status", json!({"text": "Secret"})).await;
  let alice = owner();
  let space = create_space(&app, &alice, "Team", "invite_only").await;
  let ext_id = user_id(&app, &ext).await;
  add_space_member(&app, &alice, &space, &carol_id, "member").await;
  add_space_member(&app, &alice, &space, &ext_id, "guest").await;
  let channel = create_channel(&app, &alice, &space, "client", "public").await;
  add_channel_member(&app, &alice, &channel, &carol_id).await;
  add_channel_member(&app, &alice, &channel, &ext_id).await;

  let listed = app
    .get(&ext, &format!("/presence?user_ids={carol_id},{dave_id}"))
    .await;
  assert_eq!(listed.body["presences"].as_array().unwrap().len(), 1);
  assert_eq!(status_of(&listed, 0)["text"], "Hello");
  app.stop().await;
}
