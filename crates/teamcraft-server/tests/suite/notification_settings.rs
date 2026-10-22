//! Notification preferences and their API against a real server and Postgres.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_server::testing::TestApp;

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

mod user_settings {
  use super::*;

  #[tokio::test]
  async fn defaults_come_from_the_instance_and_a_put_replaces_everything() {
    let app = app!();
    let carol = member("carol");

    let defaults = app.get(&carol, "/me/notification-settings").await;
    assert_eq!(defaults.status, StatusCode::OK, "{}", defaults.body);
    assert_eq!(
      defaults.body,
      json!({"auto_follow_mentioned_threads": true, "push_mode": "away_only", "send_read_receipts": true})
    );

    let schedule = json!({
      "utc_offset_minutes": 60,
      "windows": [{"weekdays": ["mon", "fri"], "start_minute": 1320, "end_minute": 420}],
      "allow_user_ids": [],
    });
    let body = json!({
      "auto_follow_mentioned_threads": false,
      "push_mode": "always",
      "send_read_receipts": false,
      "dnd_schedule": schedule,
    });
    let put = app.put(&carol, "/me/notification-settings", body.clone()).await;
    assert_eq!((put.status, &put.body), (StatusCode::OK, &body));
    assert_eq!(app.get(&carol, "/me/notification-settings").await.body, body);

    let cleared =
      json!({"auto_follow_mentioned_threads": true, "push_mode": "away_only", "send_read_receipts": true});
    app
      .put(&carol, "/me/notification-settings", cleared.clone())
      .await;
    assert_eq!(app.get(&carol, "/me/notification-settings").await.body, cleared);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_instance_default_for_receipts_applies_until_the_user_chooses() {
    let Some(app) =
      TestApp::start_with(|settings| settings.notifications.read_receipts_default = false).await
    else {
      return;
    };
    let carol = member("carol");
    assert_eq!(
      app.get(&carol, "/me/notification-settings").await.body["send_read_receipts"],
      false
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_settings_are_rejected() {
    let app = app!();
    let carol = member("carol");
    let base = |schedule: Value, mode: &str| {
      json!({
        "auto_follow_mentioned_threads": true,
        "push_mode": mode,
        "send_read_receipts": true,
        "dnd_schedule": schedule,
      })
    };
    let window = |weekdays: Value, start: u32, end: u32| json!({"utc_offset_minutes": 0, "windows": [{"weekdays": weekdays, "start_minute": start, "end_minute": end}]});
    for (what, body) in [
      ("unknown mode", base(Value::Null, "sometimes")),
      ("no weekdays", base(window(json!([]), 0, 60), "away_only")),
      (
        "unknown weekday",
        base(window(json!(["someday"]), 0, 60), "away_only"),
      ),
      (
        "minute out of range",
        base(window(json!(["mon"]), 0, 1440), "away_only"),
      ),
      ("empty window", base(window(json!(["mon"]), 60, 60), "away_only")),
      (
        "offset out of range",
        base(json!({"utc_offset_minutes": 900, "windows": []}), "away_only"),
      ),
    ] {
      let reply = app.put(&carol, "/me/notification-settings", body).await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{what}: {}", reply.body);
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn opting_out_of_mention_follows_keeps_mentioned_members_out_of_threads() {
    let app = app!();
    let (alice, bob, carol) = (owner(), member("bob"), member("carol"));
    let channel = channel_with(&app, &[&bob, &carol]).await;
    let (alice_device, bob_device) = (
      register_device(&app, &alice).await,
      register_device(&app, &bob).await,
    );
    let carol_id = user_id(&app, &carol).await;
    app
      .put(
        &carol,
        "/me/notification-settings",
        json!({"auto_follow_mentioned_threads": false, "push_mode": "away_only", "send_read_receipts": true}),
      )
      .await;
    let root = send(&app, &alice, &channel, &alice_device, 1).await;

    send_in(&app, &bob, &channel, &bob_device, 2, Some(&root), &[&carol_id]).await;

    let threads = app.get(&carol, "/threads").await;
    assert!(
      threads.body["threads"].as_array().unwrap().is_empty(),
      "{}",
      threads.body
    );
    app.stop().await;
  }
}

mod conversation_settings {
  use super::*;

  #[tokio::test]
  async fn level_and_mute_time_round_trip_and_show_up_in_the_membership() {
    let app = app!();
    let carol = member("carol");
    let channel = channel_with(&app, &[&carol]).await;
    let path = format!("/conversations/{channel}/notification-settings");

    let before = app.get(&carol, &path).await;
    assert_eq!(before.body, json!({"level": "mentions", "is_inherited": true}));

    let put = app
      .put(
        &carol,
        &path,
        json!({"level": "all", "muted_until": 4_102_444_800_000_i64}),
      )
      .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.body);
    assert_eq!(
      app.get(&carol, &path).await.body,
      json!({"level": "all", "muted_until": 4_102_444_800_000_i64, "is_inherited": false})
    );
    let conversation = app.get(&carol, &format!("/conversations/{channel}")).await;
    assert_eq!(
      conversation.body["my_membership"]["muted_until"],
      4_102_444_800_000_i64
    );
    assert_eq!(conversation.body["my_membership"]["notification_level"], "all");

    app.put(&carol, &path, json!({"level": "mentions"})).await;
    assert_eq!(
      app.get(&carol, &path).await.body,
      json!({"level": "mentions", "is_inherited": false})
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn only_members_with_a_known_level_can_change_it() {
    let app = app!();
    let carol = member("carol");
    let channel = channel_with(&app, &[&carol]).await;
    let path = format!("/conversations/{channel}/notification-settings");
    let eve = member("eve");

    assert_eq!(app.get(&eve, &path).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
      app.put(&eve, &path, json!({"level": "all"})).await.status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      app.put(&carol, &path, json!({"level": "loud"})).await.status,
      StatusCode::BAD_REQUEST
    );
    app.stop().await;
  }
}
