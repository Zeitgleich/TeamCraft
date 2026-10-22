//! Retention against a real server, Postgres and (for attachments) the dev Garage.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{PolicyDocument, WorkspacePolicy};
use teamcraft_server::services::retention::run_once;
use teamcraft_server::testing::{TestApp, TestUser};
use time::{Duration, OffsetDateTime};

macro_rules! app {
  () => {
    match TestApp::start().await {
      Some(app) => app,
      None => return,
    }
  };
}

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A channel of alice's with a registered device; returns (channel, alice's device).
async fn channel_of_alice(app: &TestApp, alice: &TestUser) -> (String, String) {
  let device = register_device(app, alice).await;
  let space = create_space(app, alice, "Team", "invite_only").await;
  (
    create_channel(app, alice, &space, "general", "private").await,
    device,
  )
}

async fn set_retention(
  app: &TestApp,
  user: &TestUser,
  channel: &str,
  setting: Value,
) -> teamcraft_server::testing::Reply {
  app
    .put(
      user,
      &format!("/conversations/{channel}/retention"),
      json!({"retention": setting}),
    )
    .await
}

async fn message_count(app: &TestApp, user: &TestUser, channel: &str) -> usize {
  app
    .get(user, &format!("/conversations/{channel}/messages?limit=200"))
    .await
    .body["messages"]
    .as_array()
    .unwrap()
    .len()
}

/// Signs a workspace policy as alice (the owner) with the given edits.
async fn sign_policy(
  app: &TestApp,
  alice: &TestUser,
  device: &str,
  expected: u64,
  edit: impl FnOnce(&mut Value),
) {
  let mut value = serde_json::to_value(PolicyDocument {
    version: expected + 1,
    policy: WorkspacePolicy::default(),
  })
  .unwrap();
  edit(&mut value["policy"]);
  let reply = app
    .post(
      alice,
      "/workspace/policy",
      json!({"expected_version": expected, "payload": b64(&serde_json::to_vec(&value).unwrap()), "signature": b64(b"sig"), "signer_device_id": device}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

mod settings {
  use super::*;

  #[tokio::test]
  async fn a_channel_sets_its_own_retention_and_reports_where_the_effective_value_comes_from() {
    let app = app!();
    let (alice, carol) = (owner(), member("carol"));
    let (channel, _) = channel_of_alice(&app, &alice).await;

    let initial = app
      .get(&alice, &format!("/conversations/{channel}/retention"))
      .await;
    assert_eq!(initial.body["own"], json!({}));
    assert_eq!(initial.body["keep_pinned"], true);

    let set = set_retention(
      &app,
      &alice,
      &channel,
      json!({"max_age_days": 30, "max_messages": 500}),
    )
    .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    assert_eq!(
      set.body["effective"],
      json!({"max_age_days": 30, "max_messages": 500})
    );
    // Members can't, and zero is no retention.
    assert_eq!(
      set_retention(&app, &carol, &channel, json!({"max_age_days": 1}))
        .await
        .status,
      StatusCode::NOT_FOUND
    );
    assert_eq!(
      set_retention(&app, &alice, &channel, json!({"max_age_days": 0}))
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    // Back to inheriting.
    let cleared = set_retention(&app, &alice, &channel, json!({})).await;
    assert_eq!(cleared.body["effective"], json!({}));
    app.stop().await;
  }

  #[tokio::test]
  async fn space_settings_are_inherited_by_channels_without_their_own() {
    let app = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Team", "invite_only").await;
    let channel = create_channel(&app, &alice, &space, "general", "private").await;
    let own = create_channel(&app, &alice, &space, "own", "private").await;

    let set = app
      .put(
        &alice,
        &format!("/spaces/{space}/retention"),
        json!({"retention": {"max_age_days": 60}}),
      )
      .await;
    assert_eq!(set.status, StatusCode::OK, "{}", set.body);
    let inherited = app
      .get(&alice, &format!("/conversations/{channel}/retention"))
      .await;
    assert_eq!(
      (inherited.body["own"].clone(), inherited.body["effective"].clone()),
      (json!({}), json!({"max_age_days": 60}))
    );
    set_retention(&app, &alice, &own, json!({"max_age_days": 7})).await;
    assert_eq!(
      app
        .get(&alice, &format!("/conversations/{own}/retention"))
        .await
        .body["effective"]["max_age_days"],
      7
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_workspace_limit_caps_what_owners_can_choose() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    sign_policy(&app, &alice, &device, 0, |p| {
      p["retention"]["default"] = json!({"max_age_days": 30});
      p["retention"]["limit"] = json!({"max_age_days": 90});
    })
    .await;

    // The default applies when nothing is set.
    assert_eq!(
      app
        .get(&alice, &format!("/conversations/{channel}/retention"))
        .await
        .body["effective"]["max_age_days"],
      30
    );
    assert_eq!(
      set_retention(&app, &alice, &channel, json!({"max_age_days": 90}))
        .await
        .status,
      StatusCode::OK
    );
    assert_eq!(
      set_retention(&app, &alice, &channel, json!({"max_age_days": 91}))
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    // Unlimited would exceed the limit; inheriting the (valid) default is fine.
    assert_eq!(
      set_retention(&app, &alice, &channel, json!({"max_messages": 10}))
        .await
        .status,
      StatusCode::OK
    );
    assert_eq!(
      set_retention(&app, &alice, &channel, json!({})).await.status,
      StatusCode::OK
    );
    let space = app.get(&alice, "/spaces").await.body["spaces"][0]["id"]
      .as_str()
      .unwrap()
      .to_owned();
    let too_long = app
      .put(
        &alice,
        &format!("/spaces/{space}/retention"),
        json!({"retention": {"max_age_days": 365}}),
      )
      .await;
    assert_eq!(too_long.status, StatusCode::BAD_REQUEST);
    app.stop().await;
  }
}

mod job {
  use super::*;

  #[tokio::test]
  async fn messages_older_than_the_age_limit_are_deleted_and_younger_ones_kept() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    for n in 1..=3 {
      send(&app, &alice, &channel, &device, n).await;
    }
    set_retention(&app, &alice, &channel, json!({"max_age_days": 30})).await;
    let now = OffsetDateTime::now_utc();

    let early = run_once(&app.state, now + Duration::days(10)).await.unwrap();
    assert_eq!(early.messages_deleted, 0);
    assert_eq!(message_count(&app, &alice, &channel).await, 3);

    let late = run_once(&app.state, now + Duration::days(31)).await.unwrap();
    assert_eq!(late.messages_deleted, 3);
    assert_eq!(message_count(&app, &alice, &channel).await, 0);
    // Deleted client IDs are forgotten, so the same ID can be sent again.
    let again = app
      .post(
        &alice,
        &format!("/conversations/{channel}/messages"),
        message_body(1, &device),
      )
      .await;
    assert_eq!(again.status, StatusCode::CREATED);
    app.stop().await;
  }

  #[tokio::test]
  async fn only_the_newest_n_messages_are_kept() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    let mut ids = Vec::new();
    for n in 1..=5 {
      ids.push(send(&app, &alice, &channel, &device, n).await);
    }
    set_retention(&app, &alice, &channel, json!({"max_messages": 2})).await;

    let report = run_once(&app.state, OffsetDateTime::now_utc()).await.unwrap();
    assert_eq!(report.messages_deleted, 3);
    let kept = app
      .get(&alice, &format!("/conversations/{channel}/messages"))
      .await;
    let kept_ids: Vec<_> = kept.body["messages"]
      .as_array()
      .unwrap()
      .iter()
      .map(|m| m["id"].as_str().unwrap().to_owned())
      .collect();
    assert_eq!(kept_ids, vec![ids[4].clone(), ids[3].clone()]);
    // A second run changes nothing.
    assert_eq!(
      run_once(&app.state, OffsetDateTime::now_utc())
        .await
        .unwrap()
        .messages_deleted,
      0
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_stricter_of_age_and_count_applies() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    for n in 1..=4 {
      send(&app, &alice, &channel, &device, n).await;
    }
    set_retention(
      &app,
      &alice,
      &channel,
      json!({"max_age_days": 365, "max_messages": 1}),
    )
    .await;
    run_once(&app.state, OffsetDateTime::now_utc()).await.unwrap();
    assert_eq!(message_count(&app, &alice, &channel).await, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn pinned_messages_survive_if_the_policy_keeps_them() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    let first = send(&app, &alice, &channel, &device, 1).await;
    send(&app, &alice, &channel, &device, 2).await;
    let pin = app
      .put(
        &alice,
        &format!("/conversations/{channel}/messages/{first}/pin"),
        json!({}),
      )
      .await;
    assert_eq!(pin.status, StatusCode::NO_CONTENT, "{}", pin.body);
    set_retention(&app, &alice, &channel, json!({"max_age_days": 1})).await;

    let report = run_once(&app.state, OffsetDateTime::now_utc() + Duration::days(2))
      .await
      .unwrap();
    assert_eq!(report.messages_deleted, 1);
    let left = app
      .get(&alice, &format!("/conversations/{channel}/messages"))
      .await;
    assert_eq!(left.body["messages"][0]["id"], first);

    // Unpinned, it goes with the next run.
    app
      .delete(&alice, &format!("/conversations/{channel}/messages/{first}/pin"))
      .await;
    run_once(&app.state, OffsetDateTime::now_utc() + Duration::days(2))
      .await
      .unwrap();
    assert_eq!(message_count(&app, &alice, &channel).await, 0);
    assert_eq!(
      app
        .put(
          &alice,
          &format!("/conversations/{channel}/messages/999/pin"),
          json!({})
        )
        .await
        .status,
      StatusCode::BAD_REQUEST
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_policy_that_does_not_keep_pinned_messages_deletes_them_too() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    let first = send(&app, &alice, &channel, &device, 1).await;
    app
      .put(
        &alice,
        &format!("/conversations/{channel}/messages/{first}/pin"),
        json!({}),
      )
      .await;
    sign_policy(&app, &alice, &device, 0, |p| {
      p["retention"]["keep_pinned"] = json!(false)
    })
    .await;
    set_retention(&app, &alice, &channel, json!({"max_age_days": 1})).await;

    run_once(&app.state, OffsetDateTime::now_utc() + Duration::days(2))
      .await
      .unwrap();
    assert_eq!(message_count(&app, &alice, &channel).await, 0);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_workspace_default_applies_to_channels_without_a_setting() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    send(&app, &alice, &channel, &device, 1).await;
    sign_policy(&app, &alice, &device, 0, |p| {
      p["retention"]["default"] = json!({"max_age_days": 14})
    })
    .await;

    run_once(&app.state, OffsetDateTime::now_utc() + Duration::days(13))
      .await
      .unwrap();
    assert_eq!(message_count(&app, &alice, &channel).await, 1);
    run_once(&app.state, OffsetDateTime::now_utc() + Duration::days(15))
      .await
      .unwrap();
    assert_eq!(message_count(&app, &alice, &channel).await, 0);
    app.stop().await;
  }

  #[tokio::test]
  async fn emptied_month_partitions_are_dropped_and_the_run_is_audited() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    send(&app, &alice, &channel, &device, 1).await;
    set_retention(&app, &alice, &channel, json!({"max_age_days": 1})).await;
    let now = OffsetDateTime::now_utc();
    let partition = format!("messages_{:04}_{:02}", now.year(), u8::from(now.month()));
    assert!(app.table_exists(&partition).await);

    // Two years later: the message is gone, so its month partition is empty and long over.
    let report = run_once(&app.state, now + Duration::days(730)).await.unwrap();
    assert!(report.partitions_dropped >= 1, "{report:?}");
    assert!(
      !app.table_exists(&partition).await,
      "{partition} should be dropped"
    );
    // Partitions around the (simulated) present are created so inserts keep landing in them.
    let later = now + Duration::days(730);
    assert!(
      app
        .table_exists(&format!(
          "messages_{:04}_{:02}",
          later.year(),
          u8::from(later.month())
        ))
        .await
    );

    let audit = app.get(&admin(), "/admin/audit?action=retention_run").await;
    assert_eq!(audit.body["entries"].as_array().unwrap().len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn partitions_with_messages_that_must_stay_are_not_dropped() {
    let app = app!();
    let alice = owner();
    let (channel, device) = channel_of_alice(&app, &alice).await;
    send(&app, &alice, &channel, &device, 1).await;
    let now = OffsetDateTime::now_utc();
    let partition = format!("messages_{:04}_{:02}", now.year(), u8::from(now.month()));

    // No retention anywhere: nothing is deleted, so the non-empty partition stays.
    run_once(&app.state, now + Duration::days(730)).await.unwrap();
    assert!(app.table_exists(&partition).await);
    assert_eq!(message_count(&app, &alice, &channel).await, 1);
    app.stop().await;
  }
}

mod attachments {
  use super::*;

  #[tokio::test]
  async fn attachments_past_retention_disappear_with_their_objects() {
    let Some(app) = TestApp::start_with_storage(|_| {}).await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let created = app
      .post(
        &carol,
        "/attachments",
        json!({"conversation_id": channel, "size_bytes": 5}),
      )
      .await;
    let id = created.id();
    app
      .http
      .put(created.body["upload_url"].as_str().unwrap())
      .body(b"12345".to_vec())
      .send()
      .await
      .unwrap();
    app
      .post(&carol, &format!("/attachments/{id}/complete"), json!({}))
      .await;
    let download = app.get(&carol, &format!("/attachments/{id}/download")).await;
    let url = download.body["download_url"].as_str().unwrap().to_owned();
    assert_eq!(app.http.get(&url).send().await.unwrap().status(), StatusCode::OK);

    app
      .put(
        &alice,
        &format!("/conversations/{channel}/retention"),
        json!({"retention": {"max_age_days": 30}}),
      )
      .await;
    let report = run_once(&app.state, OffsetDateTime::now_utc() + Duration::days(31))
      .await
      .unwrap();
    assert_eq!(report.attachments_deleted, 1);
    assert_eq!(
      app
        .get(&carol, &format!("/attachments/{id}/download"))
        .await
        .error_code(),
      "attachment_not_found"
    );
    assert_eq!(
      app.http.get(&url).send().await.unwrap().status(),
      StatusCode::NOT_FOUND,
      "the object is gone from storage"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn conversations_with_pinned_messages_keep_their_attachments() {
    let Some(app) = TestApp::start_with_storage(|_| {}).await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let device = register_device(&app, &alice).await;
    let message = send(&app, &alice, &channel, &device, 1).await;
    app
      .put(
        &alice,
        &format!("/conversations/{channel}/messages/{message}/pin"),
        json!({}),
      )
      .await;
    let created = app
      .post(
        &carol,
        "/attachments",
        json!({"conversation_id": channel, "size_bytes": 5}),
      )
      .await;
    let id = created.id();
    app
      .http
      .put(created.body["upload_url"].as_str().unwrap())
      .body(b"12345".to_vec())
      .send()
      .await
      .unwrap();
    app
      .post(&carol, &format!("/attachments/{id}/complete"), json!({}))
      .await;

    app
      .put(
        &alice,
        &format!("/conversations/{channel}/retention"),
        json!({"retention": {"max_age_days": 30}}),
      )
      .await;
    let report = run_once(&app.state, OffsetDateTime::now_utc() + Duration::days(31))
      .await
      .unwrap();
    assert_eq!(report.attachments_deleted, 0);
    assert_eq!(
      app
        .get(&carol, &format!("/attachments/{id}/download"))
        .await
        .status,
      StatusCode::OK
    );
    app.stop().await;
  }
}
