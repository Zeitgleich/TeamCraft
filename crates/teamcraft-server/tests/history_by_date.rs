//! "Jump to date": message IDs are snowflakes, so a time is a cursor for the `after` parameter.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use teamcraft_proto::Snowflake;
use teamcraft_server::testing::TestApp;

#[tokio::test]
async fn a_time_cursor_pages_history_from_that_moment() {
  let Some(app) = TestApp::start().await else {
    return;
  };
  let alice = owner();
  let device = register_device(&app, &alice).await;
  let space = create_space(&app, &alice, "Team", "invite_only").await;
  let channel = create_channel(&app, &alice, &space, "general", "public").await;
  let mut sent = Vec::new();
  for n in 1..=4 {
    sent.push(send(&app, &alice, &channel, &device, n).await);
    // Distinct milliseconds, so every message has its own timestamp.
    tokio::time::sleep(Duration::from_millis(20)).await;
  }

  let third = Snowflake::new(sent[2].parse().unwrap()).unwrap();
  let cursor = Snowflake::cursor_before(third.unix_millis());
  let page = app
    .get(
      &alice,
      &format!(
        "/conversations/{channel}/messages?limit=10&after={}",
        cursor.get()
      ),
    )
    .await;

  assert_eq!(page.status, StatusCode::OK, "{}", page.body);
  let ids: Vec<String> = page.body["messages"]
    .as_array()
    .unwrap()
    .iter()
    .map(|message| message["id"].as_str().unwrap().to_owned())
    .collect();
  assert_eq!(ids, vec![sent[2].clone(), sent[3].clone()]);
  app.stop().await;
}
