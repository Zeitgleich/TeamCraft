//! Derived presence: "in a call", "in a meeting" (TeamCraft meetings and calendar free/busy), the
//! admin switches and presets of the signed policy, and the away timeout.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_proto::{DeviceActivity, GatewayClientMessage, GatewayEvent, PolicyDocument, WorkspacePolicy};
use teamcraft_server::config::{CallsConfig, MailTexts, MeetingsConfig};
use teamcraft_server::meetings::caldav::BusyPeriod;
use teamcraft_server::services::presence::{demote_idle_devices, refresh_derived_statuses};
use teamcraft_server::testing::{FakeCalendar, FakeMailer, TestApp, TestUser};
use time::OffsetDateTime;

const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
const SFU_TOKEN: &str = "sfu-registration-token-for-tests";
const QUIET: Duration = Duration::from_millis(400);

fn meetings_config() -> MeetingsConfig {
  MeetingsConfig {
    public_base_url: "https://chat.example.com".into(),
    organizer_fallback_address: "meetings@example.com".into(),
    caldav: None,
    max_invitees: 5,
    guest_token_secret: "0123456789abcdef0123456789abcdef".to_owned().into(),
    guest_grace_secs: 7200,
    maybe_inbox_token: None,
    smtp: None,
    mail_texts: MailTexts::default(),
  }
}

async fn with_meetings() -> Option<(TestApp, Arc<FakeCalendar>)> {
  let calendar = Arc::new(FakeCalendar::default());
  let app = TestApp::start_with_meetings(
    &meetings_config(),
    calendar.clone(),
    Arc::new(FakeMailer::default()),
  )
  .await?;
  Some((app, calendar))
}

/// The presence of `user` as `viewer` sees it.
async fn presence_of(app: &TestApp, viewer: &TestUser, user: &str) -> Value {
  let reply = app.get(viewer, &format!("/presence?user_ids={user}")).await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  reply.body["presences"][0].clone()
}

async fn publish_policy(app: &TestApp, version: u64, edit: impl FnOnce(&mut Value)) {
  use base64::Engine;
  let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
  let alice = owner();
  let devices = app.get(&alice, "/devices").await;
  let device = match devices.body["devices"].as_array().and_then(|list| list.first()) {
    Some(device) => device["id"].as_str().unwrap().to_owned(),
    None => register_device(app, &alice).await,
  };
  let mut value = serde_json::to_value(PolicyDocument {
    version,
    policy: WorkspacePolicy::default(),
  })
  .unwrap();
  edit(&mut value["policy"]);
  let reply = app
    .post(
      &alice,
      "/workspace/policy",
      json!({
        "expected_version": version - 1,
        "payload": b64(&serde_json::to_vec(&value).unwrap()),
        "signature": b64(b"sig"),
        "signer_device_id": device,
      }),
    )
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
}

mod meetings {
  use super::*;

  #[tokio::test]
  async fn an_ongoing_meeting_shows_until_it_ends_or_is_cancelled() {
    let Some((app, _)) = with_meetings().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let alice_id = user_id(&app, &alice).await;
    let device = register_device(&app, &alice).await;
    let _socket = ws::connect(&app, &alice, &device).await;

    assert!(presence_of(&app, &carol, &alice_id).await.get("status").is_none());
    let meeting = app.post(&alice, "/meetings/instant", json!({})).await;
    assert_eq!(meeting.status, StatusCode::CREATED, "{}", meeting.body);
    refresh_derived_statuses(&app.state, OffsetDateTime::now_utc(), false)
      .await
      .unwrap();
    let shown = presence_of(&app, &carol, &alice_id).await;
    assert_eq!(shown["state"], "online");
    assert_eq!(shown["status"]["activity"], "in_meeting");
    assert!(shown["status"]["expires_at"].is_i64());

    let id = meeting.body["id"].as_str().unwrap();
    assert_eq!(
      app.delete(&alice, &format!("/meetings/{id}")).await.status,
      StatusCode::OK
    );
    refresh_derived_statuses(&app.state, OffsetDateTime::now_utc(), false)
      .await
      .unwrap();
    assert!(presence_of(&app, &carol, &alice_id).await.get("status").is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn the_calendar_of_an_online_user_marks_them_busy() {
    let Some((app, calendar)) = with_meetings().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let alice_id = user_id(&app, &alice).await;
    let device = register_device(&app, &alice).await;
    let mut socket = ws::connect(&app, &alice, &device).await;
    let carol_device = register_device(&app, &carol).await;
    let mut carol_socket = ws::connect(&app, &carol, &carol_device).await;
    channel_with(&app, &[&carol]).await;
    ws::events(&mut carol_socket, QUIET).await;
    let _ = &mut socket;

    let now = OffsetDateTime::now_utc();
    let end = now + time::Duration::minutes(30);
    calendar.set_busy(
      "alice@example.com",
      vec![
        BusyPeriod {
          start: now - time::Duration::minutes(5),
          end,
        },
        BusyPeriod {
          start: now + time::Duration::hours(3),
          end: now + time::Duration::hours(4),
        },
      ],
    );
    // The poll sees both periods; only the current one counts.
    refresh_derived_statuses(&app.state, now, true).await.unwrap();
    let shown = presence_of(&app, &carol, &alice_id).await;
    assert_eq!(shown["status"]["activity"], "in_meeting");
    assert_eq!(
      shown["status"]["expires_at"],
      i64::try_from(end.unix_timestamp_nanos() / 1_000_000).unwrap()
    );
    let events = ws::events(&mut carol_socket, QUIET).await;
    assert!(
      events.iter().any(|event| matches!(
        event,
        GatewayEvent::PresenceUpdate { maybe_status: Some(status), .. } if status.maybe_activity.is_some()
      )),
      "{events:?}"
    );

    // Free again at the next poll.
    calendar.set_busy("alice@example.com", vec![]);
    refresh_derived_statuses(&app.state, now, true).await.unwrap();
    assert!(presence_of(&app, &carol, &alice_id).await.get("status").is_none());
    app.stop().await;
  }

  #[tokio::test]
  async fn a_status_the_user_set_wins_and_offline_users_show_nothing() {
    let Some((app, calendar)) = with_meetings().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let alice_id = user_id(&app, &alice).await;
    let device = register_device(&app, &alice).await;
    let socket = ws::connect(&app, &alice, &device).await;
    let now = OffsetDateTime::now_utc();
    calendar.set_busy(
      "alice@example.com",
      vec![BusyPeriod {
        start: now - time::Duration::minutes(5),
        end: now + time::Duration::minutes(30),
      }],
    );
    refresh_derived_statuses(&app.state, now, true).await.unwrap();
    assert_eq!(
      presence_of(&app, &carol, &alice_id).await["status"]["activity"],
      "in_meeting"
    );

    app
      .put(&alice, "/me/status", json!({"emoji": "🌴", "text": "Vacation"}))
      .await;
    let shown = presence_of(&app, &carol, &alice_id).await;
    assert_eq!(shown["status"]["text"], "Vacation");
    assert!(shown["status"].get("activity").is_none());
    app.delete(&alice, "/me/status").await;
    assert_eq!(
      presence_of(&app, &carol, &alice_id).await["status"]["activity"],
      "in_meeting"
    );

    // Appearing offline hides the derived status as well.
    app
      .put(&alice, "/me/presence", json!({"manual": "invisible"}))
      .await;
    let hidden = presence_of(&app, &carol, &alice_id).await;
    assert_eq!(hidden["state"], "offline");
    assert!(hidden.get("status").is_none());
    drop(socket);
    app.stop().await;
  }
}

mod calls {
  use super::*;

  fn calls_config() -> CallsConfig {
    CallsConfig {
      sfu_url: "https://sfu.example.com:4433/call".into(),
      issuer: teamcraft_server::testing::TEST_ISSUER.into(),
      signing_key_path: PathBuf::from("unused.pem"),
      join_token_ttl_secs: 60,
      call_idle_secs: 3600,
      maybe_sfu_registration_token: Some(SFU_TOKEN.to_owned().into()),
      ring_timeout_secs: 30,
      maybe_sfu_control_url: None,
    }
  }

  async fn report(app: &TestApp, action: &str, call: &str, user: &str, device: &str) {
    let status = app
      .http
      .post(format!(
        "{}/api/internal/v1/calls/participants/{action}",
        app.base_url
      ))
      .bearer_auth(SFU_TOKEN)
      .json(&json!({"call_id": call, "user_id": user, "device_id": device}))
      .send()
      .await
      .unwrap()
      .status();
    assert_eq!(status, StatusCode::NO_CONTENT);
  }

  #[tokio::test]
  async fn people_in_a_call_show_it_until_they_leave() {
    let Some(app) = TestApp::start_with_calls_config(&calls_config(), SIGNING_KEY_PEM).await else {
      return;
    };
    let (alice, bob) = (owner(), member("bob"));
    let channel = channel_with(&app, &[&bob]).await;
    let (alice_id, alice_device) = (user_id(&app, &alice).await, register_device(&app, &alice).await);
    let _socket = ws::connect(&app, &alice, &alice_device).await;
    let bob_device = register_device(&app, &bob).await;
    let mut bob_socket = ws::connect(&app, &bob, &bob_device).await;
    ws::events(&mut bob_socket, QUIET).await;

    let token = app
      .post(
        &alice,
        &format!("/conversations/{channel}/call/join-token"),
        json!({"device_id": alice_device}),
      )
      .await;
    let call = token.body["call_id"].as_str().unwrap().to_owned();
    report(&app, "joined", &call, &alice_id, &alice_device).await;
    assert_eq!(
      presence_of(&app, &bob, &alice_id).await["status"]["activity"],
      "in_call"
    );
    let events = ws::events(&mut bob_socket, QUIET).await;
    assert!(events.iter().any(|event| matches!(
      event,
      GatewayEvent::PresenceUpdate { maybe_status: Some(status), .. }
        if status.maybe_activity == Some(teamcraft_proto::PresenceActivity::InCall)
    )));

    report(&app, "left", &call, &alice_id, &alice_device).await;
    assert!(presence_of(&app, &bob, &alice_id).await.get("status").is_none());
    app.stop().await;
  }
}

mod switches {
  use super::*;

  #[tokio::test]
  async fn statuses_can_be_switched_off_for_everybody() {
    let Some((app, calendar)) = with_meetings().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let alice_id = user_id(&app, &alice).await;
    let device = register_device(&app, &alice).await;
    let _socket = ws::connect(&app, &alice, &device).await;
    app.put(&alice, "/me/status", json!({"text": "Heads down"})).await;
    let now = OffsetDateTime::now_utc();
    calendar.set_busy(
      "alice@example.com",
      vec![BusyPeriod {
        start: now - time::Duration::minutes(1),
        end: now + time::Duration::minutes(30),
      }],
    );
    refresh_derived_statuses(&app.state, now, true).await.unwrap();
    assert!(presence_of(&app, &carol, &alice_id).await.get("status").is_some());

    publish_policy(&app, 1, |p| {
      p["presence"] = json!({"statuses_enabled": false, "custom_status_enabled": true})
    })
    .await;
    assert!(presence_of(&app, &carol, &alice_id).await.get("status").is_none());
    refresh_derived_statuses(&app.state, now, false).await.unwrap();
    assert!(presence_of(&app, &carol, &alice_id).await.get("status").is_none());
    let denied = app.put(&alice, "/me/status", json!({"text": "Again"})).await;
    assert_eq!(
      (denied.status, denied.error_code()),
      (StatusCode::FORBIDDEN, "forbidden")
    );
    // Clearing still works.
    assert_eq!(
      app.delete(&alice, "/me/status").await.status,
      StatusCode::NO_CONTENT
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn without_custom_statuses_people_choose_a_preset_that_may_end_by_itself() {
    let Some(app) = TestApp::start().await else { return };
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    publish_policy(&app, 1, |p| {
      p["presence"] = json!({
        "statuses_enabled": true,
        "custom_status_enabled": false,
        "presets": [
          {"emoji": "🍽", "text": "Lunch", "duration_minutes": 60},
          {"text": "Focus"},
        ],
      });
    })
    .await;

    let free_text = app
      .put(&carol, "/me/status", json!({"emoji": "🌴", "text": "Vacation"}))
      .await;
    assert_eq!(free_text.status, StatusCode::FORBIDDEN, "{}", free_text.body);
    let near_miss = app.put(&carol, "/me/status", json!({"text": "Lunch"})).await;
    assert_eq!(
      near_miss.status,
      StatusCode::FORBIDDEN,
      "the emoji is part of the preset"
    );

    let lunch = app
      .put(&carol, "/me/status", json!({"emoji": "🍽", "text": "Lunch"}))
      .await;
    assert_eq!(lunch.status, StatusCode::OK, "{}", lunch.body);
    let ends = lunch.body["expires_at"].as_i64().unwrap();
    let expected = (OffsetDateTime::now_utc() + time::Duration::minutes(60)).unix_timestamp() * 1000;
    assert!((ends - expected).abs() < 10_000, "{ends} vs {expected}");
    let focus = app.put(&carol, "/me/status", json!({"text": "Focus"})).await;
    assert_eq!(focus.status, StatusCode::OK);
    assert!(focus.body.get("expires_at").is_none());
    assert_eq!(
      presence_of(&app, &alice, &carol_id).await["status"]["text"],
      "Focus"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn invalid_presence_policies_are_refused() {
    let Some(app) = TestApp::start().await else { return };
    use base64::Engine;
    let alice = owner();
    let device = register_device(&app, &alice).await;
    for presence in [
      json!({"statuses_enabled": true, "custom_status_enabled": true, "away_timeout_minutes": 0}),
      json!({"statuses_enabled": true, "custom_status_enabled": true, "presets": [{"text": "  "}]}),
      json!({"statuses_enabled": true, "custom_status_enabled": true, "presets": [{"text": "x", "duration_minutes": 0}]}),
      json!({"statuses_enabled": true, "custom_status_enabled": true, "presets": (0..21).map(|n| json!({"text": format!("s{n}")})).collect::<Vec<_>>()}),
    ] {
      let mut value = serde_json::to_value(PolicyDocument {
        version: 1,
        policy: WorkspacePolicy::default(),
      })
      .unwrap();
      value["policy"]["presence"] = presence.clone();
      let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
      let reply = app
        .post(
          &alice,
          "/workspace/policy",
          json!({"expected_version": 0, "payload": b64(&serde_json::to_vec(&value).unwrap()), "signature": b64(b"s"), "signer_device_id": device}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{presence}");
    }
    app.stop().await;
  }
}

mod away_timeout {
  use super::*;

  #[tokio::test]
  async fn devices_that_stay_silent_count_as_idle_after_the_timeout() {
    let Some(app) = TestApp::start().await else { return };
    let (carol, dave) = (member("carol"), member("dave"));
    let carol_id = user_id(&app, &carol).await;
    channel_with(&app, &[&carol, &dave]).await;
    let (carol_device, dave_device) = (
      register_device(&app, &carol).await,
      register_device(&app, &dave).await,
    );
    let mut carol_socket = ws::connect(&app, &carol, &carol_device).await;
    let mut dave_socket = ws::connect(&app, &dave, &dave_device).await;
    ws::events(&mut dave_socket, QUIET).await;
    assert_eq!(presence_of(&app, &dave, &carol_id).await["state"], "online");

    // Nothing is silent for longer than an hour yet.
    let an_hour_ago = tokio::time::Instant::now()
      .checked_sub(Duration::from_secs(3600))
      .unwrap();
    assert_eq!(demote_idle_devices(&app.state, an_hour_ago).await.unwrap(), 0);

    // Dave keeps working, Carol does not: a cutoff between the two catches only Carol.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cutoff = tokio::time::Instant::now();
    tokio::time::sleep(Duration::from_millis(20)).await;
    {
      use futures_util::SinkExt;
      let active = GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Active,
      };
      dave_socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
          serde_json::to_string(&active).unwrap().into(),
        ))
        .await
        .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(demote_idle_devices(&app.state, cutoff).await.unwrap(), 1);
    assert_eq!(presence_of(&app, &dave, &carol_id).await["state"], "away");
    let events = ws::events(&mut dave_socket, QUIET).await;
    assert!(events.iter().any(|event| matches!(
      event,
      GatewayEvent::PresenceUpdate {
        state: teamcraft_proto::PresenceState::Away,
        ..
      }
    )));
    let _ = &mut carol_socket;

    // The device says it is active again: back online.
    use futures_util::SinkExt;
    let active = GatewayClientMessage::SetActivity {
      activity: DeviceActivity::Active,
    };
    carol_socket
      .send(tokio_tungstenite::tungstenite::Message::Text(
        serde_json::to_string(&active).unwrap().into(),
      ))
      .await
      .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(presence_of(&app, &dave, &carol_id).await["state"], "online");
    app.stop().await;
  }
}
