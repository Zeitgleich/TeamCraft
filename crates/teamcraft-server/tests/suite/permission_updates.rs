//! Changed rights reach the clients live (issue #758): a channel role, a space role or a space's
//! permission scheme changes, and the affected members hear about each conversation whose
//! `my_permissions` changed, so their clients fetch it again.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::GatewayEvent;
use teamcraft_server::testing::{TestApp, TestUser};

const QUIET: Duration = Duration::from_millis(500);
const MANAGE: &str = "manage_channel_properties";

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

#[tokio::test]
async fn a_channel_role_change_reaches_the_member_with_the_new_permissions() {
  let app = app!();
  let (alice, carol) = (owner(), member("carol"));
  let carol_id = user_id(&app, &carol).await;
  let space = create_space(&app, &alice, "Team", "invite_only").await;
  add_space_member(&app, &alice, &space, &carol_id, "member").await;
  let channel = create_channel(&app, &alice, &space, "news", "private").await;
  add_channel_member(&app, &alice, &channel, &carol_id).await;
  assert!(!holds(&app, &carol, &channel, MANAGE).await);
  let device = register_device(&app, &carol).await;
  let mut socket = ws::connect(&app, &carol, &device).await;

  let promote = app
    .patch(
      &alice,
      &format!("/conversations/{channel}/members/{carol_id}"),
      json!({"role": "admin"}),
    )
    .await;
  assert_eq!(promote.status, StatusCode::NO_CONTENT, "{}", promote.body);

  let events = ws::events(&mut socket, QUIET).await;
  assert_eq!(updated(&events), vec![channel.clone()], "{events:?}");
  assert!(holds(&app, &carol, &channel, MANAGE).await);
  app.stop().await;
}

#[tokio::test]
async fn a_space_scheme_change_updates_every_conversation_of_that_space_only() {
  let app = app!();
  let (alice, bob, carol, dave) = (owner(), admin(), member("carol"), member("dave"));
  let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
  let (team, other) = (
    create_space(&app, &alice, "Team", "invite_only").await,
    create_space(&app, &alice, "Other", "invite_only").await,
  );
  for space in [&team, &other] {
    add_space_member(&app, &alice, space, &carol_id, "member").await;
  }
  add_space_member(&app, &alice, &team, &dave_id, "member").await;
  let (news, chat, elsewhere) = (
    create_channel(&app, &alice, &team, "news", "private").await,
    create_channel(&app, &alice, &team, "chat", "private").await,
    create_channel(&app, &alice, &other, "elsewhere", "private").await,
  );
  for channel in [&news, &chat, &elsewhere] {
    add_channel_member(&app, &alice, channel, &carol_id).await;
  }
  add_channel_member(&app, &alice, &news, &dave_id).await;
  let (carol_device, dave_device) = (
    register_device(&app, &carol).await,
    register_device(&app, &dave).await,
  );
  let mut carol_socket = ws::connect(&app, &carol, &carol_device).await;
  let mut dave_socket = ws::connect(&app, &dave, &dave_device).await;

  let reply = app
    .put(
      &bob,
      &format!("/admin/spaces/{team}/permission-scheme"),
      json!({"changes": [
        {"role": "member", "permission": "add_reaction", "effect": "revoke"},
        {"role": "member", "permission": "create_channel", "effect": "revoke"},
      ]}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);

  let carol_events = ws::events(&mut carol_socket, QUIET).await;
  let mut carol_updated = updated(&carol_events);
  carol_updated.sort();
  let mut expected = vec![news.clone(), chat.clone()];
  expected.sort();
  assert_eq!(
    carol_updated, expected,
    "not the other space's channel: {carol_events:?}"
  );
  assert!(space_updated(&carol_events, &team), "{carol_events:?}");
  let dave_events = ws::events(&mut dave_socket, QUIET).await;
  assert_eq!(updated(&dave_events), vec![news.clone()], "{dave_events:?}");

  assert!(!holds(&app, &carol, &news, "add_reaction").await);
  assert!(holds(&app, &carol, &elsewhere, "add_reaction").await);
  let spaces = app.get(&carol, "/spaces").await;
  let can_create = |space: &str| {
    spaces.body["spaces"]
      .as_array()
      .unwrap()
      .iter()
      .find(|found| found["id"] == space)
      .unwrap()["can_create_channels"]
      .clone()
  };
  assert_eq!(can_create(&team), json!(false));
  assert_eq!(can_create(&other), json!(true));
  app.stop().await;
}

#[tokio::test]
async fn a_space_role_change_updates_that_members_conversations_for_them_alone() {
  let app = app!();
  let (alice, carol, dave) = (owner(), member("carol"), member("dave"));
  let (carol_id, dave_id) = (user_id(&app, &carol).await, user_id(&app, &dave).await);
  let space = create_space(&app, &alice, "Team", "invite_only").await;
  let channel = create_channel(&app, &alice, &space, "news", "private").await;
  for id in [&carol_id, &dave_id] {
    add_space_member(&app, &alice, &space, id, "member").await;
    add_channel_member(&app, &alice, &channel, id).await;
  }
  assert!(!holds(&app, &carol, &channel, MANAGE).await);
  let (carol_device, dave_device) = (
    register_device(&app, &carol).await,
    register_device(&app, &dave).await,
  );
  let mut carol_socket = ws::connect(&app, &carol, &carol_device).await;
  let mut dave_socket = ws::connect(&app, &dave, &dave_device).await;

  let promote = app
    .patch(
      &alice,
      &format!("/spaces/{space}/members/{carol_id}"),
      json!({"role": "space_admin"}),
    )
    .await;
  assert_eq!(promote.status, StatusCode::OK, "{}", promote.body);

  let carol_events = ws::events(&mut carol_socket, QUIET).await;
  assert_eq!(updated(&carol_events), vec![channel.clone()], "{carol_events:?}");
  let dave_events = ws::events(&mut dave_socket, QUIET).await;
  assert!(
    updated(&dave_events).is_empty(),
    "dave's rights stayed: {dave_events:?}"
  );
  assert!(space_updated(&dave_events, &space), "{dave_events:?}");
  assert!(holds(&app, &carol, &channel, MANAGE).await);

  // The same role again changes nobody's rights.
  let again = app
    .patch(
      &alice,
      &format!("/spaces/{space}/members/{carol_id}"),
      json!({"role": "space_admin"}),
    )
    .await;
  assert_eq!(again.status, StatusCode::OK, "{}", again.body);
  assert!(updated(&ws::events(&mut carol_socket, QUIET).await).is_empty());
  app.stop().await;
}

/// The conversations a `conversation_update` named, in order.
fn updated(events: &[GatewayEvent]) -> Vec<String> {
  events
    .iter()
    .filter_map(|event| match event {
      GatewayEvent::ConversationUpdate { conversation_id } => Some(conversation_id.to_string()),
      _ => None,
    })
    .collect()
}

fn space_updated(events: &[GatewayEvent], space: &str) -> bool {
  events
    .iter()
    .any(|event| matches!(event, GatewayEvent::SpaceUpdate { space_id } if space_id.to_string() == space))
}

/// Whether the server tells `user` they hold `permission` in the conversation.
async fn holds(app: &TestApp, user: &TestUser, conversation: &str, permission: &str) -> bool {
  let reply = app.get(user, &format!("/conversations/{conversation}")).await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["my_permissions"]
    .as_array()
    .unwrap()
    .iter()
    .any(|granted| granted == permission)
}
