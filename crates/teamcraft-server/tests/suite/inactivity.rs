//! Inactive channels, archiving a space with its channels, and idle guest accounts (issue #212).
//! Time is the job's `now`: a channel "ages" because the run is given a later clock.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::Arc;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{ConversationId, PolicyDocument, WorkspacePolicy};
use teamcraft_server::services::guests::expire_once;
use teamcraft_server::services::inactivity::{InactivityRun, deactivate_idle_guest_accounts, run_once};
use teamcraft_server::testing::{FakeMailer, TestApp, TestUser};
use time::{Duration, OffsetDateTime};

macro_rules! app {
  () => {
    match start().await {
      Some(started) => started,
      None => return,
    }
  };
}

/// A server that can mail, so owner notices can be delivered.
async fn start() -> Option<(TestApp, Arc<FakeMailer>)> {
  let mailer = Arc::new(FakeMailer::default());
  let app = TestApp::start_with_mailer(
    |settings| {
      settings.mail.public_base_url = "https://chat.example.com".into();
      settings.local_accounts.is_enabled = true;
      settings.local_accounts.maybe_session_secret =
        Some("0123456789abcdef0123456789abcdef-secret".to_owned().into());
      settings.local_accounts.password_memory_kib = 64;
      settings.local_accounts.password_iterations = 1;
    },
    mailer.clone(),
  )
  .await?;
  Some((app, mailer))
}

fn b64(bytes: &[u8]) -> String {
  use base64::Engine;
  base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn at(days: i64) -> OffsetDateTime {
  OffsetDateTime::now_utc() + Duration::days(days)
}

/// Signs a workspace policy as alice (the owner) with the given edits; returns its version.
async fn sign_policy(
  app: &TestApp,
  alice: &TestUser,
  device: &str,
  expected: u64,
  edit: impl FnOnce(&mut Value),
) -> u64 {
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
  expected + 1
}

/// Notice after 30 quiet days, archive 10 days later, at most `max` channels per run.
async fn enable(app: &TestApp, alice: &TestUser, device: &str, version: u64, max: u32) -> u64 {
  sign_policy(app, alice, device, version, |policy| {
    policy["inactivity"] = json!({
      "is_enabled": true,
      "notice_after_days": 30,
      "archive_after_more_days": 10,
      "snooze_days": 60,
      "max_per_run": max,
    });
  })
  .await
}

async fn is_archived(app: &TestApp, user: &TestUser, channel: &str) -> bool {
  app.get(user, &format!("/conversations/{channel}")).await.body["is_archived"]
    .as_bool()
    .unwrap()
}

async fn audit_actions(app: &TestApp) -> Vec<Value> {
  app.get(&admin(), "/admin/audit?limit=200").await.body["entries"]
    .as_array()
    .unwrap()
    .clone()
}

fn count_of(entries: &[Value], action: &str) -> usize {
  entries.iter().filter(|entry| entry["action"] == action).count()
}

fn id_of(channel: &str) -> ConversationId {
  channel.parse().unwrap()
}

mod policy_off {
  use super::*;

  #[tokio::test]
  async fn nothing_happens_until_an_admin_turns_it_on() {
    let (app, mailer) = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;

    // Years later, with the default policy: no notice, no archive, no audit entry, no mail.
    for days in [60, 400, 4000] {
      assert_eq!(
        run_once(&app.state, at(days)).await.unwrap(),
        InactivityRun::default()
      );
    }
    assert!(!is_archived(&app, &alice, &channel).await);
    assert!(mailer.sent().is_empty());
    let entries = audit_actions(&app).await;
    assert!(
      entries
        .iter()
        .all(|e| !e["action"].as_str().unwrap().contains("inactivity")),
      "{entries:?}"
    );

    // A policy that names it but leaves it off does the same.
    let device = register_device(&app, &alice).await;
    sign_policy(&app, &alice, &device, 0, |policy| {
      policy["inactivity"] =
        json!({"is_enabled": false, "notice_after_days": 1, "archive_after_more_days": 1});
    })
    .await;
    assert_eq!(
      run_once(&app.state, at(400)).await.unwrap(),
      InactivityRun::default()
    );
    assert!(!is_archived(&app, &alice, &channel).await);
    app.stop().await;
  }
}

mod notice_and_archive {
  use super::*;

  #[tokio::test]
  async fn the_owner_is_told_and_the_channel_is_archived_later_with_audit_entries() {
    let (app, mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "zebra", "public").await;
    enable(&app, &alice, &device, 0, 10).await;

    // Quiet for 29 days: too early.
    assert_eq!(
      run_once(&app.state, at(29)).await.unwrap(),
      InactivityRun::default()
    );

    // Quiet for 31 days: the owner hears, once. The mail names no channel.
    let run = run_once(&app.state, at(31)).await.unwrap();
    assert_eq!(run.noticed, 1);
    assert_eq!(run.archived, 0);
    let sent = mailer.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to_address, "alice@example.com");
    assert!(
      sent[0].text.contains("10 days") && sent[0].text.contains("https://chat.example.com"),
      "{}",
      sent[0].text
    );
    assert!(!sent[0].text.contains("zebra") && !sent[0].subject.contains("zebra"));
    assert_eq!(
      run_once(&app.state, at(32)).await.unwrap(),
      InactivityRun::default(),
      "once"
    );
    assert!(!is_archived(&app, &alice, &channel).await);

    // The console lists when it is archived.
    let listed = app.get(&admin(), "/admin/conversations?limit=50").await;
    let row = listed.body["conversations"]
      .as_array()
      .unwrap()
      .iter()
      .find(|row| row["id"] == channel.as_str())
      .unwrap();
    let notified = row["inactivity"]["notified_at"].as_i64().unwrap();
    assert_eq!(
      row["inactivity"]["archive_at"].as_i64().unwrap(),
      notified + 10 * 86_400_000
    );
    assert_eq!(row["inactivity"]["is_archived_by_policy"], false);

    // Nine days after the notice: still open. Eleven: archived.
    assert_eq!(run_once(&app.state, at(40)).await.unwrap().archived, 0);
    let run = run_once(&app.state, at(42)).await.unwrap();
    assert_eq!(run.archived, 1);
    assert!(is_archived(&app, &alice, &channel).await);
    // Nobody posts into it any more.
    let refused = app
      .post(
        &alice,
        &format!("/conversations/{channel}/messages"),
        message_body(1, &device),
      )
      .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);

    // The audit log says what happened, without an actor.
    let entries = audit_actions(&app).await;
    for action in ["channel_inactivity_notice", "channel_auto_archived"] {
      let entry = entries
        .iter()
        .find(|e| e["action"] == action)
        .unwrap_or_else(|| panic!("{action}: {entries:?}"));
      assert!(entry["actor_user_id"].is_null(), "{entry}");
      assert_eq!(entry["target_kind"], "conversation");
      assert_eq!(entry["target_id"], channel.as_str());
    }
    let detail: Value = serde_json::from_str(
      entries
        .iter()
        .find(|e| e["action"] == "channel_auto_archived")
        .unwrap()["detail"]
        .as_str()
        .unwrap(),
    )
    .unwrap();
    assert!(detail["quiet_days"].as_i64().unwrap() >= 41, "{detail}");

    // Members see the usual "archived" line, and an admin restores the channel; a restored channel is
    // not archived again at the next run.
    let events = app
      .get(&alice, &format!("/conversations/{channel}/system-events"))
      .await
      .body
      .to_string();
    assert!(events.contains("archived"), "{events}");
    let listed = app.get(&admin(), "/admin/conversations?limit=50").await;
    let row = listed.body["conversations"]
      .as_array()
      .unwrap()
      .iter()
      .find(|row| row["id"] == channel.as_str())
      .unwrap();
    assert_eq!(row["inactivity"]["is_archived_by_policy"], true, "{row}");
    let restored = app
      .patch(
        &alice,
        &format!("/conversations/{channel}"),
        json!({"is_archived": false}),
      )
      .await;
    assert_eq!(restored.status, StatusCode::OK, "{}", restored.body);
    assert_eq!(
      run_once(&app.state, at(43)).await.unwrap(),
      InactivityRun::default()
    );
    assert!(!is_archived(&app, &alice, &channel).await);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_message_after_the_notice_ends_the_quiet_spell() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    enable(&app, &alice, &device, 0, 10).await;

    // A notice an hour ago, then a message.
    let noticed = OffsetDateTime::now_utc() - Duration::hours(1);
    assert!(
      app
        .state
        .database
        .record_inactivity_notice(id_of(&channel), noticed, None)
        .await
        .unwrap()
    );
    send(&app, &alice, &channel, &device, 1).await;
    // Far enough that the archive would be due, if the notice stood.
    let run = run_once(&app.state, at(20)).await.unwrap();
    assert_eq!(run.archived, 0);
    assert!(!is_archived(&app, &alice, &channel).await);
    // The notice is forgotten: the next quiet spell starts over with a notice.
    assert_eq!(
      run_once(&app.state, at(20)).await.unwrap(),
      InactivityRun::default()
    );
    let run = run_once(&app.state, at(31)).await.unwrap();
    assert_eq!((run.noticed, run.archived), (1, 0));
    app.stop().await;
  }

  #[tokio::test]
  async fn a_message_racing_the_archiving_wins() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = id_of(&create_channel(&app, &alice, &space, "old", "public").await);
    let noticed = OffsetDateTime::now_utc() - Duration::hours(1);
    assert!(
      app
        .state
        .database
        .record_inactivity_notice(channel, noticed, None)
        .await
        .unwrap()
    );
    send(&app, &alice, &channel.to_string(), &device, 1).await;
    // The job read the channel before the message and archives on the old notice: refused.
    assert!(
      !app
        .state
        .database
        .archive_inactive_channel(channel, noticed, at(20))
        .await
        .unwrap()
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_notice_that_nobody_received_archives_nothing() {
    // No mail server: the owners can't be told, so the channel is left alone.
    let Some(app) = TestApp::start().await else { return };
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    enable(&app, &alice, &device, 0, 10).await;
    for days in [31, 45, 90] {
      assert_eq!(
        run_once(&app.state, at(days)).await.unwrap(),
        InactivityRun::default()
      );
    }
    assert!(!is_archived(&app, &alice, &channel).await);
    assert_eq!(
      count_of(&audit_actions(&app).await, "channel_inactivity_notice"),
      0
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn an_old_notice_is_repeated_instead_of_acted_on() {
    // The policy was off (or the server down) for months: switching it on again must not archive on
    // a notice from long ago.
    let (app, mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = id_of(&create_channel(&app, &alice, &space, "old", "public").await);
    enable(&app, &alice, &device, 0, 10).await;
    // A notice recorded just after the channel was made, which stands as a notice of long ago once
    // the run's clock is far ahead.
    let old = OffsetDateTime::now_utc() + Duration::seconds(1);
    assert!(
      app
        .state
        .database
        .record_inactivity_notice(channel, old, None)
        .await
        .unwrap()
    );
    let run = run_once(&app.state, at(100)).await.unwrap();
    assert_eq!((run.noticed, run.archived), (1, 0));
    assert_eq!(mailer.sent().len(), 1);
    assert!(!is_archived(&app, &alice, &channel.to_string()).await);
    // Only after the new notice's time does it archive.
    assert_eq!(run_once(&app.state, at(111)).await.unwrap().archived, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn notices_only_never_archive() {
    let (app, mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    sign_policy(&app, &alice, &device, 0, |policy| {
      policy["inactivity"] =
        json!({"is_enabled": true, "notice_after_days": 30, "archive_after_more_days": 0});
    })
    .await;
    assert_eq!(run_once(&app.state, at(31)).await.unwrap().noticed, 1);
    assert!(
      mailer.sent()[0].text.contains("Nothing happens"),
      "{}",
      mailer.sent()[0].text
    );
    assert_eq!(run_once(&app.state, at(900)).await.unwrap().archived, 0);
    assert!(!is_archived(&app, &alice, &channel).await);
    app.stop().await;
  }

  #[tokio::test]
  async fn archived_spaces_channels_and_dms_are_left_alone() {
    let (app, _mailer) = app!();
    let alice = owner();
    let bob = admin();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    create_channel(&app, &alice, &space, "old", "public").await;
    let archived = create_space(&app, &alice, "Gone", "open").await;
    let in_archived = create_channel(&app, &alice, &archived, "x", "public").await;
    let reply = app
      .patch(
        &alice,
        &format!("/spaces/{archived}"),
        json!({"is_archived": true}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let bob_id = user_id(&app, &bob).await;
    let dm = app.post(&alice, "/dms", json!({"user_id": bob_id})).await;
    assert!(dm.status.is_success(), "{}", dm.body);
    enable(&app, &alice, &device, 0, 10).await;
    let run = run_once(&app.state, at(31)).await.unwrap();
    assert_eq!(run.noticed, 1, "only the open channel of the open space");
    assert!(is_archived(&app, &alice, &in_archived).await);
    app.stop().await;
  }
}

mod snooze {
  use super::*;

  #[tokio::test]
  async fn a_snooze_defers_the_notice_the_archive_and_starts_the_time_over() {
    let (app, mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    enable(&app, &alice, &device, 0, 10).await;
    assert_eq!(run_once(&app.state, at(31)).await.unwrap().noticed, 1);

    // The owner keeps the channel open (the policy's 60 days).
    let reply = app
      .post(
        &alice,
        &format!("/conversations/{channel}/inactivity/snooze"),
        json!({}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body["snoozed_until"].as_i64().unwrap() > at(59).unix_timestamp() * 1000);

    // Where the archive would have been due, and well after: nothing, no new notice either.
    for days in [42, 55, 59] {
      assert_eq!(
        run_once(&app.state, at(days)).await.unwrap(),
        InactivityRun::default(),
        "day {days}"
      );
    }
    assert!(!is_archived(&app, &alice, &channel).await);

    // The snooze is over and the channel is still quiet: a fresh notice, then the full time again.
    let run = run_once(&app.state, at(61)).await.unwrap();
    assert_eq!((run.noticed, run.archived), (1, 0));
    assert_eq!(mailer.sent().len(), 2);
    assert_eq!(run_once(&app.state, at(65)).await.unwrap().archived, 0);
    assert_eq!(run_once(&app.state, at(72)).await.unwrap().archived, 1);

    let entries = audit_actions(&app).await;
    let entry = entries
      .iter()
      .find(|e| e["action"] == "channel_inactivity_snoozed")
      .unwrap();
    assert_eq!(entry["actor_user_id"], user_id(&app, &alice).await.as_str());
    app.stop().await;
  }

  #[tokio::test]
  async fn snoozing_ahead_of_the_notice_works_too_and_the_length_is_checked() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    enable(&app, &alice, &device, 0, 10).await;
    let path = format!("/conversations/{channel}/inactivity/snooze");
    for body in [json!({"days": 0}), json!({"days": 366})] {
      assert_eq!(
        app.post(&alice, &path, body).await.status,
        StatusCode::BAD_REQUEST
      );
    }
    assert_eq!(
      app.post(&alice, &path, json!({"days": 100})).await.status,
      StatusCode::OK
    );
    assert_eq!(
      run_once(&app.state, at(99)).await.unwrap(),
      InactivityRun::default()
    );
    assert_eq!(run_once(&app.state, at(101)).await.unwrap().noticed, 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn who_may_snooze() {
    let (app, _mailer) = app!();
    let (alice, bob, carol, dave, gus) = (owner(), admin(), member("carol"), member("dave"), guest("gus"));
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    let (carol_id, dave_id, gus_id) = (
      user_id(&app, &carol).await,
      user_id(&app, &dave).await,
      user_id(&app, &gus).await,
    );
    user_id(&app, &bob).await;
    for id in [&carol_id, &dave_id] {
      add_space_member(&app, &alice, &space, id, "member").await;
    }
    add_space_member(&app, &alice, &space, &gus_id, "guest").await;
    for id in [&carol_id, &dave_id] {
      add_channel_member(&app, &alice, &channel, id).await;
    }
    let promoted = app
      .patch(
        &alice,
        &format!("/conversations/{channel}/members/{carol_id}"),
        json!({"role": "admin"}),
      )
      .await;
    assert!(
      promoted.status.is_success(),
      "{} {}",
      promoted.status,
      promoted.body
    );
    let path = format!("/conversations/{channel}/inactivity/snooze");

    // Owner, workspace admin and the channel's own admin may; a plain member, a guest and a stranger
    // may not.
    for (who, expected) in [
      (&alice, StatusCode::OK),
      (&bob, StatusCode::OK),
      (&carol, StatusCode::OK),
    ] {
      let reply = app.post(who, &path, json!({"days": 5})).await;
      assert_eq!(reply.status, expected, "{}: {}", who.username, reply.body);
    }
    for who in [&dave, &gus, &member("eve")] {
      let reply = app.post(who, &path, json!({"days": 5})).await;
      assert!(
        matches!(reply.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
        "{}: {} {}",
        who.username,
        reply.status,
        reply.body
      );
    }
    let entries = audit_actions(&app).await;
    assert_eq!(count_of(&entries, "channel_inactivity_snoozed"), 3, "{entries:?}");
    app.stop().await;
  }
}

mod limits {
  use super::*;

  #[tokio::test]
  async fn a_run_handles_at_most_the_limit_and_says_so() {
    let (app, mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let mut channels = Vec::new();
    for name in ["a", "b", "c", "d", "e"] {
      channels.push(create_channel(&app, &alice, &space, name, "public").await);
    }
    enable(&app, &alice, &device, 0, 2).await;

    // Switching the policy on notices two channels per run, not the whole instance.
    for expected in [2, 2, 1] {
      let run = run_once(&app.state, at(31)).await.unwrap();
      assert_eq!(run.noticed, expected);
      assert_eq!(run.archived, 0);
    }
    assert_eq!(mailer.sent().len(), 5);
    assert_eq!(
      run_once(&app.state, at(31)).await.unwrap(),
      InactivityRun::default()
    );

    // Two archived per run.
    let mut archived = 0;
    for expected in [2, 2, 1] {
      let run = run_once(&app.state, at(45)).await.unwrap();
      assert_eq!(run.archived, expected);
      archived += run.archived;
    }
    assert_eq!(archived, 5);
    let mut open = 0;
    for channel in &channels {
      if !is_archived(&app, &alice, channel).await {
        open += 1;
      }
    }
    assert_eq!(open, 0);

    // The audit log shows that the limit held things back.
    let entries = audit_actions(&app).await;
    assert!(count_of(&entries, "inactivity_run_limited") >= 2, "{entries:?}");
    assert_eq!(count_of(&entries, "channel_auto_archived"), 5);
    app.stop().await;
  }
}

mod retention_step {
  use super::*;

  async fn with_delete(app: &TestApp, alice: &TestUser, device: &str, days: Option<u32>) {
    sign_policy(app, alice, device, 0, |policy| {
      let mut inactivity =
        json!({"is_enabled": true, "notice_after_days": 30, "archive_after_more_days": 10});
      if let Some(days) = days {
        inactivity["delete_after_archived_days"] = json!(days);
      }
      policy["inactivity"] = inactivity;
    })
    .await;
  }

  #[tokio::test]
  async fn nothing_is_deleted_without_the_setting() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    send(&app, &alice, &channel, &device, 1).await;
    with_delete(&app, &alice, &device, None).await;
    run_once(&app.state, at(400)).await.unwrap();
    assert_eq!(
      run_once(&app.state, at(405)).await.unwrap().archived,
      0,
      "10 days after the notice"
    );
    assert_eq!(run_once(&app.state, at(411)).await.unwrap().archived, 1);
    for days in [500, 2000, 6000] {
      let run = run_once(&app.state, at(days)).await.unwrap();
      assert_eq!((run.deleted, run.delete_warned), (0, 0));
    }
    let reply = app
      .get(&alice, &format!("/conversations/{channel}/messages?limit=10"))
      .await;
    assert_eq!(
      reply.body["messages"].as_array().unwrap().len(),
      1,
      "{}",
      reply.body
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn an_archived_channel_is_warned_then_deleted_and_the_audit_log_says_so() {
    let (app, mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    with_delete(&app, &alice, &device, Some(20)).await;
    run_once(&app.state, at(31)).await.unwrap();
    assert_eq!(run_once(&app.state, at(42)).await.unwrap().archived, 1);
    let before = mailer.sent().len();

    // 20 days after the archive, 7 days of warning: the warning goes out first.
    let run = run_once(&app.state, at(42 + 12)).await.unwrap();
    assert_eq!((run.delete_warned, run.deleted), (0, 0), "too early");
    let run = run_once(&app.state, at(42 + 14)).await.unwrap();
    assert_eq!((run.delete_warned, run.deleted), (1, 0));
    let warning = mailer.sent().split_off(before);
    assert_eq!(warning.len(), 1);
    assert!(warning[0].text.contains("deleted"), "{}", warning[0].text);
    assert_eq!(
      run_once(&app.state, at(42 + 18)).await.unwrap().deleted,
      0,
      "the warning stands first"
    );
    let run = run_once(&app.state, at(42 + 21)).await.unwrap();
    assert_eq!(run.deleted, 1);
    assert_eq!(
      app.get(&alice, &format!("/conversations/{channel}")).await.status,
      StatusCode::NOT_FOUND
    );

    let entries = audit_actions(&app).await;
    for action in ["channel_inactivity_delete_notice", "channel_inactivity_deleted"] {
      let entry = entries
        .iter()
        .find(|e| e["action"] == action)
        .unwrap_or_else(|| panic!("{action}"));
      assert!(entry["actor_user_id"].is_null());
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn a_restored_channel_and_a_channel_archived_by_hand_are_never_deleted() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let by_policy = create_channel(&app, &alice, &space, "policy", "public").await;
    let by_hand = create_channel(&app, &alice, &space, "hand", "public").await;
    let hand = app
      .patch(
        &alice,
        &format!("/conversations/{by_hand}"),
        json!({"is_archived": true}),
      )
      .await;
    assert_eq!(hand.status, StatusCode::OK, "{}", hand.body);
    with_delete(&app, &alice, &device, Some(20)).await;
    run_once(&app.state, at(31)).await.unwrap();
    assert_eq!(run_once(&app.state, at(42)).await.unwrap().archived, 1);
    // The warning goes out, then an admin restores the channel before the deletion.
    assert_eq!(run_once(&app.state, at(56)).await.unwrap().delete_warned, 1);
    let reply = app
      .patch(
        &alice,
        &format!("/conversations/{by_policy}"),
        json!({"is_archived": false}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    for days in [70, 200] {
      assert_eq!(run_once(&app.state, at(days)).await.unwrap().deleted, 0);
    }
    for channel in [&by_policy, &by_hand] {
      assert_eq!(
        app.get(&alice, &format!("/conversations/{channel}")).await.status,
        StatusCode::OK
      );
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn a_snooze_defers_the_deletion_and_a_stale_warning_is_repeated() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "old", "public").await;
    with_delete(&app, &alice, &device, Some(20)).await;
    run_once(&app.state, at(31)).await.unwrap();
    run_once(&app.state, at(42)).await.unwrap();
    assert_eq!(run_once(&app.state, at(56)).await.unwrap().delete_warned, 1);
    let snoozed = app
      .post(
        &alice,
        &format!("/conversations/{channel}/inactivity/snooze"),
        json!({"days": 100}),
      )
      .await;
    assert_eq!(snoozed.status, StatusCode::OK, "{}", snoozed.body);
    assert_eq!(
      run_once(&app.state, at(90)).await.unwrap(),
      InactivityRun::default()
    );
    // After the snooze: warned afresh, deleted only after that warning.
    let run = run_once(&app.state, at(160)).await.unwrap();
    assert_eq!((run.delete_warned, run.deleted), (1, 0));
    assert_eq!(run_once(&app.state, at(168)).await.unwrap().deleted, 1);
    app.stop().await;
  }
}

mod space_archive {
  use super::*;

  async fn state_of(app: &TestApp, user: &TestUser, channels: &[&String]) -> Vec<bool> {
    let mut states = Vec::new();
    for channel in channels {
      states.push(is_archived(app, user, channel).await);
    }
    states
  }

  #[tokio::test]
  async fn archiving_a_space_archives_its_channels_and_restoring_reverts_exactly_those() {
    let (app, _mailer) = app!();
    let alice = owner();
    let space = create_space(&app, &alice, "Team", "open").await;
    let a = create_channel(&app, &alice, &space, "a", "public").await;
    let b = create_channel(&app, &alice, &space, "b", "private").await;
    let by_hand = create_channel(&app, &alice, &space, "c", "public").await;
    let hand = app
      .patch(
        &alice,
        &format!("/conversations/{by_hand}"),
        json!({"is_archived": true}),
      )
      .await;
    assert_eq!(hand.status, StatusCode::OK, "{}", hand.body);

    let reply = app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": true}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["is_archived"], true);
    assert_eq!(
      state_of(&app, &alice, &[&a, &b, &by_hand]).await,
      [true, true, true]
    );
    // The same request again changes nothing and is not audited twice.
    let again = app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": true}))
      .await;
    assert_eq!(again.status, StatusCode::OK);

    let entries = audit_actions(&app).await;
    assert_eq!(count_of(&entries, "space_archived"), 1);
    let entry = entries.iter().find(|e| e["action"] == "space_archived").unwrap();
    assert_eq!(entry["actor_user_id"], user_id(&app, &alice).await.as_str());
    let detail: Value = serde_json::from_str(entry["detail"].as_str().unwrap()).unwrap();
    assert_eq!(
      detail["channels"], 2,
      "the channel archived by hand is not counted"
    );

    // Restoring brings back the two the space archived; the one archived by hand stays archived.
    let reply = app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": false}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["is_archived"], false);
    assert_eq!(
      state_of(&app, &alice, &[&a, &b, &by_hand]).await,
      [false, false, true]
    );
    let entries = audit_actions(&app).await;
    let entry = entries.iter().find(|e| e["action"] == "space_restored").unwrap();
    let detail: Value = serde_json::from_str(entry["detail"].as_str().unwrap()).unwrap();
    assert_eq!(
      (detail["channels"].as_i64(), detail["left_archived"].as_i64()),
      (Some(2), Some(0))
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn members_are_told_about_each_channel_and_only_space_managers_may() {
    let (app, _mailer) = app!();
    let (alice, carol) = (owner(), member("carol"));
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "a", "public").await;
    let carol_id = user_id(&app, &carol).await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    add_channel_member(&app, &alice, &channel, &carol_id).await;
    let refused = app
      .patch(&carol, &format!("/spaces/{space}"), json!({"is_archived": true}))
      .await;
    assert!(
      matches!(refused.status, StatusCode::FORBIDDEN | StatusCode::NOT_FOUND),
      "{} {}",
      refused.status,
      refused.body
    );
    assert!(!is_archived(&app, &alice, &channel).await);
    let reply = app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": true}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let events = app
      .get(&carol, &format!("/conversations/{channel}/system-events"))
      .await
      .body
      .to_string();
    assert!(events.contains("archived"), "{events}");
    app.stop().await;
  }

  #[tokio::test]
  async fn a_channel_nobody_can_run_stays_archived_until_it_has_an_owner() {
    let (app, _mailer) = app!();
    let (alice, carol) = (owner(), member("carol"));
    let space = create_space(&app, &alice, "Team", "open").await;
    let channel = create_channel(&app, &alice, &space, "a", "public").await;
    let carol_id = user_id(&app, &carol).await;
    add_space_member(&app, &alice, &space, &carol_id, "member").await;
    add_channel_member(&app, &alice, &channel, &carol_id).await;
    // The space is archived. Leaving an archived channel is allowed to anybody, also its last admin:
    // carol, a plain member, is left with a channel nobody can run.
    let reply = app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": true}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let alice_id = user_id(&app, &alice).await;
    let left = app
      .delete(&alice, &format!("/conversations/{channel}/members/{alice_id}"))
      .await;
    assert!(left.status.is_success(), "{} {}", left.status, left.body);
    let reply = app
      .patch(&alice, &format!("/spaces/{space}"), json!({"is_archived": false}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let entries = audit_actions(&app).await;
    let entry = entries.iter().find(|e| e["action"] == "space_restored").unwrap();
    let detail: Value = serde_json::from_str(entry["detail"].as_str().unwrap()).unwrap();
    assert_eq!(
      (detail["channels"].as_i64(), detail["left_archived"].as_i64()),
      (Some(0), Some(1)),
      "{detail}"
    );
    assert!(is_archived(&app, &carol, &channel).await);
    app.stop().await;
  }
}

mod guest_accounts {
  use super::*;

  /// A local guest account that has signed in (a password, a device) and is a guest of `space` for a
  /// day.
  async fn local_guest(app: &TestApp, alice: &TestUser, space: &str, email: &str) -> String {
    let reply = app
      .post(
        alice,
        &format!("/spaces/{space}/local-guests"),
        json!({"email": email, "display_name": "Gus Guest"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    let id = reply.body["user"]["id"].as_str().unwrap().to_owned();
    let user_id: i64 = id.parse().unwrap();
    let pool = app.sql_pool();
    teamcraft_db::testing::sqlx::query("UPDATE local_accounts SET password_hash = 'hash' WHERE user_id = $1")
      .bind(user_id)
      .execute(pool)
      .await
      .unwrap();
    teamcraft_db::testing::sqlx::query(
      "INSERT INTO devices (id, user_id, name, public_key, cross_signature) VALUES ($1, $2, 'laptop', 'k', 's')",
    )
    .bind(user_id + 1_000_000)
    .bind(user_id)
    .execute(pool)
    .await
    .unwrap();
    let expires_at = (OffsetDateTime::now_utc() + Duration::days(1)).unix_timestamp() * 1000;
    let added = app
      .post(
        alice,
        &format!("/spaces/{space}/members"),
        json!({"user_id": id, "role": "guest", "expires_at": expires_at}),
      )
      .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    id
  }

  async fn is_deactivated(app: &TestApp, id: &str) -> bool {
    let reply = app
      .get(&admin(), "/admin/users?query=Gus&status=all&limit=50")
      .await;
    reply.body["users"]
      .as_array()
      .unwrap()
      .iter()
      .find(|user| user["id"] == id)
      .unwrap()["is_deactivated"]
      .as_bool()
      .unwrap()
  }

  #[tokio::test]
  async fn off_by_default_and_a_grace_period_after_the_last_space() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    user_id(&app, &admin()).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let gus = local_guest(&app, &alice, &space, "gus@partner.test").await;

    // The guest's membership expires (90 days by default): the account remains, as before.
    assert_eq!(expire_once(&app.state, at(2)).await.unwrap(), 1);
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(100)).await.unwrap(),
      0
    );
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(1000))
        .await
        .unwrap(),
      0
    );
    assert!(!is_deactivated(&app, &gus).await);

    // With a period of 30 days: the clock starts at the first run that sees the guest without a
    // space, and the account is deactivated 30 days after that.
    sign_policy(&app, &alice, &device, 0, |policy| {
      policy["guests"] = json!({"deactivate_accounts_after_days": 30});
    })
    .await;
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(110)).await.unwrap(),
      0
    );
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(139)).await.unwrap(),
      0
    );
    assert!(!is_deactivated(&app, &gus).await);
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(141)).await.unwrap(),
      1
    );
    assert!(is_deactivated(&app, &gus).await);
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(300)).await.unwrap(),
      0,
      "once"
    );

    let entries = app
      .get(&admin(), "/admin/audit?limit=200&action=user_deactivated")
      .await
      .body["entries"]
      .as_array()
      .unwrap()
      .clone();
    let entry = entries.iter().find(|e| e["target_id"] == gus.as_str()).unwrap();
    assert!(entry["actor_user_id"].is_null());
    assert!(
      entry["detail"].as_str().unwrap().contains("without a space"),
      "{entry}"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_guest_who_gets_a_space_again_starts_over_and_members_are_never_touched() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    let gus = local_guest(&app, &alice, &space, "gus@partner.test").await;
    let carol = member("carol");
    user_id(&app, &carol).await;
    user_id(&app, &admin()).await;
    sign_policy(&app, &alice, &device, 0, |policy| {
      policy["guests"] = json!({"deactivate_accounts_after_days": 30});
    })
    .await;
    assert_eq!(expire_once(&app.state, at(2)).await.unwrap(), 1);
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(100)).await.unwrap(),
      0
    );
    // Added to a space on day 120: the clock stops.
    add_space_member(&app, &alice, &space, &gus, "guest").await;
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(125)).await.unwrap(),
      0
    );
    assert_eq!(
      deactivate_idle_guest_accounts(&app.state, at(200)).await.unwrap(),
      0
    );
    assert!(!is_deactivated(&app, &gus).await);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_guest_whose_invite_waits_for_the_first_sign_in_is_left_alone() {
    let (app, _mailer) = app!();
    let alice = owner();
    let device = register_device(&app, &alice).await;
    let space = create_space(&app, &alice, "Client", "invite_only").await;
    // Created, never activated: no space yet, the invite is applied at the first sign-in.
    let reply = app
      .post(
        &alice,
        &format!("/spaces/{space}/local-guests"),
        json!({"email": "new@partner.test", "display_name": "New Guest"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    sign_policy(&app, &alice, &device, 0, |policy| {
      policy["guests"] = json!({"deactivate_accounts_after_days": 30});
    })
    .await;
    for days in [1, 40, 400] {
      assert_eq!(
        deactivate_idle_guest_accounts(&app.state, at(days))
          .await
          .unwrap(),
        0
      );
    }
    app.stop().await;
  }
}
