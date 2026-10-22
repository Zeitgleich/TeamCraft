//! Many accounts appear at once, and one account sends several requests at once on its first
//! sight: sign-in provisioning (user row, default memberships, device registration) never answers
//! 5xx. Regression for the intermittent enrollment `500` under parallel runs (docs/acceptance.md).
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use futures_util::future::join_all;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::{ConversationId, Id, Snowflake, SpaceId};
use teamcraft_server::testing::{TestApp, TestUser};

const ACCOUNTS: usize = 24;
const REQUESTS_PER_ACCOUNT: usize = 4;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn first_requests_of_new_accounts_in_parallel_never_fail() {
  let Some(mut app) = TestApp::start().await else { return };
  let (space, channel) = default_workspace(&mut app).await;
  assert!(!space.is_empty());

  let users: Vec<TestUser> = (0..ACCOUNTS).map(|n| member(&format!("racer{n}"))).collect();
  let replies = join_all(users.iter().flat_map(|user| {
    let app = &app;
    (0..REQUESTS_PER_ACCOUNT).map(move |attempt| async move {
      // Mixed first requests: a profile read, the account itself and a device registration.
      match attempt % 3 {
        0 => app.get(user, "/me").await,
        1 => app.get(user, "/spaces").await,
        _ => {
          app
            .post(
              user,
              "/devices",
              json!({"name": format!("laptop {attempt}"), "public_key": "cHVibGlj", "cross_signature": "c2ln"}),
            )
            .await
        },
      }
    })
  }))
  .await;
  for reply in &replies {
    assert!(
      reply.status.is_success(),
      "status {}: {}",
      reply.status,
      reply.body
    );
  }

  // Everybody got the defaults exactly once.
  for user in &users {
    let me = user_id(&app, user).await;
    let members = app
      .get(&owner(), &format!("/conversations/{channel}/members"))
      .await;
    let count = members.body["members"]
      .as_array()
      .unwrap()
      .iter()
      .filter(|member| member["user"]["id"] == me.as_str())
      .count();
    assert_eq!(count, 1, "{} in {}", me, members.body);
  }
  app.stop().await;
}

async fn default_workspace(app: &mut TestApp) -> (String, String) {
  let alice = owner();
  let space = create_space(app, &alice, "Company", "invite_only").await;
  let channel = create_channel(app, &alice, &space, "general", "public").await;
  let (space_raw, channel_raw) = (space.parse::<i64>().unwrap(), channel.parse::<i64>().unwrap());
  app.reconfigure(|settings| {
    settings.policy.default_spaces = vec![SpaceId::from_snowflake(Snowflake::new(space_raw).unwrap())];
    settings.policy.default_channels = vec![ConversationId::from_snowflake(Snowflake::new(channel_raw).unwrap())];
  });
  assert_eq!(app.get(&alice, "/me").await.status, StatusCode::OK);
  (space, channel)
}
