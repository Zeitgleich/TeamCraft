//! Default spaces and channels for new members (policy `default_spaces` / `default_channels`).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use reqwest::StatusCode;
use teamcraft_proto::{ConversationId, Id, Snowflake, SpaceId};
use teamcraft_server::testing::{TestApp, TestUser};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

struct Workspace {
  space: String,
  general: String,
  secret: String,
  other: String,
}

#[tokio::test]
async fn a_new_member_joins_the_default_space_and_its_public_default_channels() {
  let mut app = app!();
  let workspace = workspace(&mut app).await;
  let bob = member("bob");

  assert_eq!(app.get(&bob, "/spaces").await.status, StatusCode::OK);

  assert!(is_space_member(&app, &bob, &workspace.space).await);
  assert!(is_channel_member(&app, &bob, &workspace.general).await);
  // Private channels and channels that are not listed stay closed.
  assert!(!is_channel_member(&app, &bob, &workspace.secret).await);
  assert!(!is_channel_member(&app, &bob, &workspace.other).await);
  app.stop().await;
}

#[tokio::test]
async fn only_the_configured_ids_count_not_names() {
  let mut app = app!();
  let workspace = workspace(&mut app).await;
  // Anyone may create a space with the same name; it is not a default.
  let lookalike = create_space(&app, &owner(), "Company", "invite_only").await;
  let bob = member("bob");

  assert_eq!(app.get(&bob, "/spaces").await.status, StatusCode::OK);

  assert!(is_space_member(&app, &bob, &workspace.space).await);
  assert!(!is_space_member(&app, &bob, &lookalike).await);
  app.stop().await;
}

#[tokio::test]
async fn defaults_apply_once_so_leaving_sticks() {
  let mut app = app!();
  let workspace = workspace(&mut app).await;
  let bob = member("bob");
  assert_eq!(app.get(&bob, "/spaces").await.status, StatusCode::OK);
  let bob_id = user_id(&app, &bob).await;

  let removed = app
    .delete(
      &owner(),
      &format!("/conversations/{}/members/{bob_id}", workspace.general),
    )
    .await;
  assert!(removed.status.is_success(), "{}", removed.body);
  assert_eq!(app.get(&bob, "/spaces").await.status, StatusCode::OK);

  assert!(!is_channel_member(&app, &bob, &workspace.general).await);
  app.stop().await;
}

#[tokio::test]
async fn guests_get_no_defaults() {
  let mut app = app!();
  let workspace = workspace(&mut app).await;
  let visitor = guest("visitor");

  assert_eq!(app.get(&visitor, "/spaces").await.status, StatusCode::OK);

  assert!(!is_space_member(&app, &visitor, &workspace.space).await);
  app.stop().await;
}

/// A space with two public channels and a private one; the policy defaults to the space and to
/// "general", "secret" (private, so it stays closed) by ID.
async fn workspace(app: &mut TestApp) -> Workspace {
  let alice = owner();
  let space = create_space(app, &alice, "Company", "invite_only").await;
  let workspace = Workspace {
    general: create_channel(app, &alice, &space, "general", "public").await,
    secret: create_channel(app, &alice, &space, "secret", "private").await,
    other: create_channel(app, &alice, &space, "random", "public").await,
    space,
  };
  let id = |text: &str| text.parse::<i64>().unwrap();
  let (space, general, secret) = (id(&workspace.space), id(&workspace.general), id(&workspace.secret));
  app.reconfigure(|settings| {
    settings.policy.default_spaces = vec![space_id(space), space_id(99)];
    settings.policy.default_channels = vec![channel_id(general), channel_id(secret)];
  });
  workspace
}

fn space_id(raw: i64) -> SpaceId {
  SpaceId::from_snowflake(Snowflake::new(raw).unwrap())
}

fn channel_id(raw: i64) -> ConversationId {
  ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
}

async fn is_space_member(app: &TestApp, user: &TestUser, space: &str) -> bool {
  let spaces = app.get(user, "/spaces").await;
  spaces.body["spaces"]
    .as_array()
    .unwrap()
    .iter()
    .any(|entry| entry["id"] == space)
}

async fn is_channel_member(app: &TestApp, user: &TestUser, channel: &str) -> bool {
  let me = user_id(app, user).await;
  let members = app
    .get(&owner(), &format!("/conversations/{channel}/members"))
    .await;
  assert_eq!(members.status, StatusCode::OK, "{}", members.body);
  members.body["members"]
    .as_array()
    .unwrap()
    .iter()
    .any(|entry| entry["user"]["id"] == me.as_str())
}
