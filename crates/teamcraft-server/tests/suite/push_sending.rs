//! Push sending: routing rules, encrypted wake-ups and Web Push against a local fake push service.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode as HttpStatus};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::push_fakes;
use common::*;
use futures_util::{SinkExt, StreamExt};
use p256::SecretKey;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_proto::{DeviceActivity, GatewayClientMessage, PushKind, PushProvider, PushWakeUp};
use teamcraft_server::auth::BoxFuture;
use teamcraft_server::push::crypto::{VapidKey, decrypt_web_push, open_wakeup};
use teamcraft_server::push::{
  PushDispatcher, PushError, PushMessage, PushSender, PushTarget, Urgency, WebPushSender,
};
use teamcraft_server::testing::{TestApp, TestUser};
use tokio_tungstenite::tungstenite::Message;

const PUSH_KEY_BYTES: [u8; 32] = [9; 32];

fn push_key() -> String {
  STANDARD.encode(PUSH_KEY_BYTES)
}

#[derive(Debug, Clone)]
struct Recorded {
  provider: PushProvider,
  device: String,
  wake_up: PushWakeUp,
  urgency: Urgency,
}

#[derive(Clone, Default)]
struct Recorder {
  seen: Arc<Mutex<Vec<Recorded>>>,
}

impl PushSender for Recorder {
  fn send<'a>(
    &'a self,
    target: &'a PushTarget,
    message: &'a PushMessage,
  ) -> BoxFuture<'a, Result<(), PushError>> {
    Box::pin(async move {
      let wake_up = open_wakeup(&target.push_key, &message.sealed)?;
      self.seen.lock().unwrap().push(Recorded {
        provider: target.provider.clone(),
        device: target.device.to_string(),
        wake_up,
        urgency: message.urgency,
      });
      Ok(())
    })
  }
}

impl Recorder {
  fn dispatcher(&self) -> PushDispatcher {
    PushDispatcher::new(8)
      .with_sender(&PushProvider::Apns, Arc::new(self.clone()))
      .with_sender(&PushProvider::Fcm, Arc::new(self.clone()))
      .with_sender(&PushProvider::UnifiedPush, Arc::new(self.clone()))
  }

  /// Waits for `count` pushes, then a little longer to catch unexpected extras.
  async fn wait_for(&self, count: usize) -> Vec<Recorded> {
    for _ in 0..60 {
      if self.seen.lock().unwrap().len() >= count {
        break;
      }
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    self.seen.lock().unwrap().clone()
  }
}

async fn start(recorder: &Recorder) -> Option<TestApp> {
  TestApp::start_with_push(|_| {}, recorder.dispatcher()).await
}

async fn register_push_device(app: &TestApp, user: &TestUser, provider: &str) -> String {
  let device = register_device(app, user).await;
  let reply = app
    .put(
      user,
      &format!("/devices/{device}/push"),
      json!({"provider": provider, "token": "t", "push_key": push_key()}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
  device
}

async fn set_level(app: &TestApp, user: &TestUser, channel: &str, level: &str) {
  let id = user_id(app, user).await;
  let reply = app
    .patch(
      user,
      &format!("/conversations/{channel}/members/{id}"),
      json!({"notification_level": level}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
}

/// alice (sender, with a push device) and carol (recipient with a push device) share a channel.
struct Scene {
  channel: String,
  alice: TestUser,
  carol: TestUser,
  alice_device: String,
  carol_push_device: String,
}

async fn scene(app: &TestApp) -> Scene {
  let (alice, carol) = (owner(), member("carol"));
  let channel = channel_with(app, &[&carol]).await;
  let alice_device = register_push_device(app, &alice, "apns").await;
  let carol_push_device = register_push_device(app, &carol, "fcm").await;
  Scene {
    channel,
    alice,
    carol,
    alice_device,
    carol_push_device,
  }
}

mod routing {
  use super::*;

  #[tokio::test]
  async fn the_wake_up_is_sealed_with_the_push_key_and_names_the_message_without_content() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    set_level(&app, &s.carol, &s.channel, "all").await;

    let message = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    let push = &pushes[0];
    assert_eq!(
      (push.provider.clone(), push.device.clone()),
      (PushProvider::Fcm, s.carol_push_device.clone())
    );
    assert_eq!(push.wake_up.kind, PushKind::Message);
    assert_eq!(push.wake_up.conversation_id.to_string(), s.channel);
    assert_eq!(push.wake_up.maybe_message_id.unwrap().to_string(), message);
    assert_eq!(
      push.wake_up.maybe_sender_id.unwrap().to_string(),
      user_id(&app, &s.alice).await
    );
    assert!(!push.wake_up.is_urgent);
    assert_eq!(push.urgency, Urgency::Normal);
    app.stop().await;
  }

  #[tokio::test]
  async fn senders_non_members_revoked_devices_and_repeats_get_nothing() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    set_level(&app, &s.carol, &s.channel, "all").await;
    let outsider = member("erin");
    register_push_device(&app, &outsider, "apns").await;
    let revoked = register_push_device(&app, &s.carol, "unifiedpush").await;
    app.delete(&s.carol, &format!("/devices/{revoked}")).await;

    send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    // Resending the same client ID stores nothing new, so nothing is pushed again.
    app
      .post(
        &s.alice,
        &format!("/conversations/{}/messages", s.channel),
        message_body(1, &s.alice_device),
      )
      .await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(
      pushes.iter().map(|p| p.device.clone()).collect::<Vec<_>>(),
      vec![s.carol_push_device.clone()]
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn mention_channels_only_push_for_mentions_and_urgent_messages_say_so() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let carol_id = user_id(&app, &s.carol).await;

    send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    assert!(
      recorder.wait_for(0).await.is_empty(),
      "the default level is mentions"
    );

    let mut body = message_body(2, &s.alice_device);
    body["mentioned_user_ids"] = json!([carol_id]);
    body["is_urgent"] = json!(true);
    app
      .post(&s.alice, &format!("/conversations/{}/messages", s.channel), body)
      .await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].wake_up.kind, PushKind::Mention);
    assert!(pushes[0].wake_up.is_urgent);
    assert_eq!(pushes[0].urgency, Urgency::High);
    app.stop().await;
  }

  #[tokio::test]
  async fn integration_channels_push_only_for_mentions_even_at_level_all() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    set_level(&app, &s.carol, &s.channel, "all").await;
    let carol_id = user_id(&app, &s.carol).await;
    let flagged = app
      .patch(
        &s.alice,
        &format!("/conversations/{}", s.channel),
        json!({"is_integration": true}),
      )
      .await;
    assert_eq!(flagged.status, StatusCode::OK, "{}", flagged.body);

    send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    assert!(
      recorder.wait_for(0).await.is_empty(),
      "plain log output doesn't push"
    );

    let mut alert = message_body(2, &s.alice_device);
    alert["mentioned_user_ids"] = json!([carol_id]);
    app
      .post(&s.alice, &format!("/conversations/{}/messages", s.channel), alert)
      .await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(pushes.len(), 1, "an alert that mentions carol does");
    assert_eq!(pushes[0].wake_up.kind, PushKind::Mention);
    app.stop().await;
  }

  #[tokio::test]
  async fn direct_messages_push_like_mentions() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let carol_id = user_id(&app, &s.carol).await;
    let dm = app
      .post(&s.alice, "/dms", json!({"user_id": carol_id}))
      .await
      .id();

    send(&app, &s.alice, &dm, &s.alice_device, 1).await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(pushes.len(), 1);
    assert_eq!(pushes[0].wake_up.kind, PushKind::Mention);
    app.stop().await;
  }

  #[tokio::test]
  async fn muted_channels_and_do_not_disturb_suppress_pushes() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let carol_id = user_id(&app, &s.carol).await;
    let mention = |n: u8| {
      let mut body = message_body(n, &s.alice_device);
      body["mentioned_user_ids"] = json!([carol_id]);
      body["is_urgent"] = json!(true);
      body
    };
    let path = format!("/conversations/{}/messages", s.channel);

    set_level(&app, &s.carol, &s.channel, "none").await;
    app.post(&s.alice, &path, mention(1)).await;
    assert!(
      recorder.wait_for(0).await.is_empty(),
      "muted, even for urgent mentions"
    );

    set_level(&app, &s.carol, &s.channel, "all").await;
    app.put(&s.carol, "/me/presence", json!({"manual": "dnd"})).await;
    app.post(&s.alice, &path, mention(2)).await;
    assert!(recorder.wait_for(0).await.is_empty(), "do not disturb");

    app.put(&s.carol, "/me/presence", json!({"manual": "auto"})).await;
    app.post(&s.alice, &path, mention(3)).await;
    assert_eq!(recorder.wait_for(1).await.len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_active_device_suppresses_pushes_an_idle_one_does_not_and_connected_devices_are_skipped() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    set_level(&app, &s.carol, &s.channel, "all").await;
    let desktop = register_push_device(&app, &s.carol, "unifiedpush").await;

    let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
    socket.next().await;
    let identify = GatewayClientMessage::Identify {
      token: app.token(&s.carol),
      device_id: serde_json::from_value(json!(desktop)).unwrap(),
      activity: DeviceActivity::Active,
    };
    socket
      .send(Message::Text(serde_json::to_string(&identify).unwrap().into()))
      .await
      .unwrap();
    socket.next().await;

    send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    assert!(
      recorder.wait_for(0).await.is_empty(),
      "carol is active on her desktop"
    );

    let idle = GatewayClientMessage::SetActivity {
      activity: DeviceActivity::Idle,
    };
    socket
      .send(Message::Text(serde_json::to_string(&idle).unwrap().into()))
      .await
      .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(&app, &s.alice, &s.channel, &s.alice_device, 2).await;
    let pushes = recorder.wait_for(1).await;
    // Only the phone: the desktop has a live connection and gets the event directly.
    assert_eq!(
      pushes.iter().map(|p| p.device.clone()).collect::<Vec<_>>(),
      vec![s.carol_push_device.clone()]
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn deactivated_users_are_not_pushed_to() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    set_level(&app, &s.carol, &s.channel, "all").await;
    let carol_id = user_id(&app, &s.carol).await;
    app
      .post(
        &admin(),
        &format!("/admin/users/{carol_id}/deactivate"),
        json!({}),
      )
      .await;

    send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    assert!(recorder.wait_for(0).await.is_empty());
    app.stop().await;
  }
}

mod preferences {
  use super::*;

  /// A DND schedule that covers the whole week, around the clock.
  fn always_dnd(allowed: &[String]) -> serde_json::Value {
    let every_day = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    json!({
      "utc_offset_minutes": 0,
      "windows": [
        {"weekdays": every_day, "start_minute": 0, "end_minute": 1439},
        {"weekdays": every_day, "start_minute": 1439, "end_minute": 0},
      ],
      "allow_user_ids": allowed,
    })
  }

  async fn put_settings(app: &TestApp, user: &TestUser, push_mode: &str, schedule: serde_json::Value) {
    let reply = app
      .put(
        user,
        "/me/notification-settings",
        json!({
          "auto_follow_mentioned_threads": true,
          "push_mode": push_mode,
          "send_read_receipts": true,
          "dnd_schedule": schedule,
        }),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  }

  async fn mention(app: &TestApp, s: &Scene, n: u8) {
    let mut body = message_body(n, &s.alice_device);
    body["mentioned_user_ids"] = json!([user_id(app, &s.carol).await]);
    let reply = app
      .post(&s.alice, &format!("/conversations/{}/messages", s.channel), body)
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  }

  #[tokio::test]
  async fn muting_until_a_time_suppresses_pushes_until_it_passes() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let path = format!("/conversations/{}/notification-settings", s.channel);
    let in_one_hour = unix_millis_from_now(3_600_000);
    let an_hour_ago = unix_millis_from_now(-3_600_000);

    app
      .put(
        &s.carol,
        &path,
        json!({"level": "all", "muted_until": in_one_hour}),
      )
      .await;
    mention(&app, &s, 1).await;
    assert!(recorder.wait_for(0).await.is_empty(), "muted for an hour");

    app
      .put(
        &s.carol,
        &path,
        json!({"level": "all", "muted_until": an_hour_ago}),
      )
      .await;
    mention(&app, &s, 2).await;
    assert_eq!(recorder.wait_for(1).await.len(), 1, "the mute passed");
    app.stop().await;
  }

  #[tokio::test]
  async fn the_dnd_schedule_silences_everyone_except_allowed_senders() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let alice_id = user_id(&app, &s.alice).await;

    put_settings(&app, &s.carol, "away_only", always_dnd(&[])).await;
    mention(&app, &s, 1).await;
    assert!(recorder.wait_for(0).await.is_empty(), "scheduled do not disturb");

    put_settings(&app, &s.carol, "away_only", always_dnd(&[alice_id])).await;
    mention(&app, &s, 2).await;
    assert_eq!(recorder.wait_for(1).await.len(), 1, "alice is allowed through");
    app.stop().await;
  }

  #[tokio::test]
  async fn replies_in_followed_threads_push_in_mention_only_channels() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let root = send(&app, &s.alice, &s.channel, &s.alice_device, 1).await;
    let carol_device = &s.carol_push_device;

    send_in(&app, &s.carol, &s.channel, carol_device, 2, Some(&root), &[]).await;
    let to_alice = recorder.wait_for(1).await;
    assert_eq!(to_alice.len(), 1, "alice wrote the root, so she follows");
    assert_eq!(to_alice[0].device, s.alice_device);
    assert_eq!(to_alice[0].wake_up.kind, PushKind::Message);

    // Carol follows her own reply; a top-level message of alice's still doesn't push.
    send(&app, &s.alice, &s.channel, &s.alice_device, 3).await;
    send_in(&app, &s.alice, &s.channel, &s.alice_device, 4, Some(&root), &[]).await;
    let all = recorder.wait_for(2).await;
    assert_eq!(all.len(), 2, "{all:?}");
    assert_eq!(all[1].device, *carol_device);
    app.stop().await;
  }

  #[tokio::test]
  async fn the_always_push_mode_pushes_while_another_device_is_active() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let _socket = ws::connect(&app, &s.carol, &desktop).await;

    mention(&app, &s, 1).await;
    assert!(
      recorder.wait_for(0).await.is_empty(),
      "away_only: carol is active"
    );

    put_settings(&app, &s.carol, "always", serde_json::Value::Null).await;
    mention(&app, &s, 2).await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(
      pushes.iter().map(|p| p.device.clone()).collect::<Vec<_>>(),
      vec![s.carol_push_device.clone()]
    );
    app.stop().await;
  }

  fn unix_millis_from_now(offset_millis: i64) -> i64 {
    let now = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_millis();
    i64::try_from(now).unwrap() + offset_millis
  }
}

mod mobile_delay {
  use super::*;

  async fn start_delayed(recorder: &Recorder, delay_secs: u64) -> Option<TestApp> {
    TestApp::start_with_push(
      |settings| settings.notifications.mobile_push_delay_secs = delay_secs,
      recorder.dispatcher(),
    )
    .await
  }

  async fn mention(app: &TestApp, s: &Scene, n: u8) -> String {
    let id = user_id(app, &s.carol).await;
    let mut body = message_body(n, &s.alice_device);
    body["mentioned_user_ids"] = json!([id]);
    let reply = app
      .post(&s.alice, &format!("/conversations/{}/messages", s.channel), body)
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    reply.id()
  }

  #[tokio::test]
  async fn an_active_desktop_holds_the_phone_back_and_a_read_message_is_never_pushed() {
    let recorder = Recorder::default();
    let Some(app) = start_delayed(&recorder, 1).await else {
      return;
    };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let _socket = ws::connect(&app, &s.carol, &desktop).await;

    // Unread after the delay: the phone gets its push, but not at once.
    mention(&app, &s, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
      recorder.seen.lock().unwrap().is_empty(),
      "held back while the desktop is active"
    );
    let pushes = recorder.wait_for(1).await;
    assert_eq!(
      pushes.iter().map(|p| p.device.clone()).collect::<Vec<_>>(),
      vec![s.carol_push_device.clone()]
    );

    // Read on the desktop in the meantime: nothing follows.
    let message = mention(&app, &s, 2).await;
    let read = app
      .put(
        &s.carol,
        &format!("/conversations/{}/read-position", s.channel),
        json!({"message_id": message}),
      )
      .await;
    assert_eq!(read.status, StatusCode::NO_CONTENT, "{}", read.body);
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert_eq!(
      recorder.seen.lock().unwrap().len(),
      1,
      "the read message was not pushed"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn a_delay_of_zero_pushes_at_once_and_idle_desktops_never_delay() {
    let recorder = Recorder::default();
    let Some(app) = start_delayed(&recorder, 0).await else {
      return;
    };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let _socket = ws::connect(&app, &s.carol, &desktop).await;
    mention(&app, &s, 1).await;
    // With delay 0 an active desktop keeps pushes away like before.
    assert!(recorder.wait_for(0).await.is_empty());
    app.stop().await;

    let recorder = Recorder::default();
    let Some(app) = start_delayed(&recorder, 30).await else {
      return;
    };
    let s = scene(&app).await;
    mention(&app, &s, 1).await;
    // Nobody is at a desktop: the phone is pushed immediately.
    assert_eq!(recorder.wait_for(1).await.len(), 1);
    app.stop().await;
  }
}

mod urgent {
  use teamcraft_server::services::urgent::run_due;
  use time::{Duration as TimeDuration, OffsetDateTime};

  use super::*;

  async fn start_urgent(recorder: &Recorder) -> Option<TestApp> {
    TestApp::start_with_push(
      |settings| {
        settings.notifications.urgent_repeat_interval_secs = 60;
        settings.notifications.urgent_max_pushes = 3;
      },
      recorder.dispatcher(),
    )
    .await
  }

  /// alice sends an urgent DM to carol and returns (dm, message).
  async fn urgent_dm(app: &TestApp, s: &Scene, n: u8) -> (String, String) {
    let carol_id = user_id(app, &s.carol).await;
    let dm = app
      .post(&s.alice, "/dms", json!({"user_id": carol_id}))
      .await
      .id();
    let mut body = message_body(n, &s.alice_device);
    body["is_urgent"] = json!(true);
    let reply = app
      .post(&s.alice, &format!("/conversations/{dm}/messages"), body)
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
    (dm, reply.id())
  }

  fn later(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::now_utc() + TimeDuration::seconds(seconds)
  }

  #[tokio::test]
  async fn unacknowledged_urgent_dms_are_pushed_again_up_to_the_limit() {
    let recorder = Recorder::default();
    let Some(app) = start_urgent(&recorder).await else {
      return;
    };
    let s = scene(&app).await;
    urgent_dm(&app, &s, 1).await;
    assert_eq!(recorder.wait_for(1).await.len(), 1, "the first push is immediate");

    assert_eq!(run_due(&app.state, later(30)).await.unwrap(), 0, "not due yet");
    assert_eq!(run_due(&app.state, later(61)).await.unwrap(), 1);
    assert_eq!(recorder.wait_for(2).await.len(), 2);
    assert_eq!(run_due(&app.state, later(61 + 61)).await.unwrap(), 1);
    assert_eq!(recorder.wait_for(3).await.len(), 3);
    assert_eq!(
      run_due(&app.state, later(600)).await.unwrap(),
      0,
      "three pushes in total"
    );

    let pushes = recorder.wait_for(3).await;
    assert!(
      pushes
        .iter()
        .all(|p| p.wake_up.is_urgent && p.wake_up.kind == PushKind::Mention)
    );
    assert!(pushes.iter().all(|p| p.device == s.carol_push_device));
    app.stop().await;
  }

  #[tokio::test]
  async fn acknowledging_stops_the_repeats_and_tells_the_sender() {
    let recorder = Recorder::default();
    let Some(app) = start_urgent(&recorder).await else {
      return;
    };
    let s = scene(&app).await;
    let (dm, message) = urgent_dm(&app, &s, 1).await;
    recorder.wait_for(1).await;
    let mut alice_socket = ws::connect(&app, &s.alice, &s.alice_device).await;
    ws::events(&mut alice_socket, Duration::from_millis(300)).await;
    let path = format!("/conversations/{dm}/messages/{message}/acknowledgement");

    let ack = app.put(&s.carol, &path, json!({})).await;
    assert_eq!(ack.status, StatusCode::NO_CONTENT, "{}", ack.body);
    assert_eq!(
      app.put(&s.carol, &path, json!({})).await.status,
      StatusCode::NO_CONTENT,
      "idempotent"
    );

    assert_eq!(run_due(&app.state, later(3600)).await.unwrap(), 0);
    assert_eq!(
      recorder.wait_for(1).await.len(),
      1,
      "no repeat after the acknowledgement"
    );
    let events = ws::events(&mut alice_socket, Duration::from_millis(400)).await;
    let acks: Vec<_> = events
      .iter()
      .map(|event| serde_json::to_value(event).unwrap())
      .filter(|json| json["type"] == "message_ack")
      .collect();
    assert_eq!(acks.len(), 1, "announced once: {acks:?}");
    assert_eq!(acks[0]["message_id"], message.as_str());
    assert_eq!(acks[0]["user_id"], user_id(&app, &s.carol).await.as_str());

    let listed = app.get(&s.alice, &format!("{path}s")).await;
    assert_eq!(listed.body["acknowledgements"].as_array().unwrap().len(), 1);
    assert_eq!(
      listed.body["acknowledgements"][0]["user_id"],
      user_id(&app, &s.carol).await.as_str()
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn only_urgent_messages_of_others_in_my_conversations_can_be_acknowledged() {
    let recorder = Recorder::default();
    let Some(app) = start_urgent(&recorder).await else {
      return;
    };
    let s = scene(&app).await;
    let (dm, message) = urgent_dm(&app, &s, 1).await;
    let plain = send(&app, &s.alice, &dm, &s.alice_device, 2).await;
    let eve = member("eve");

    let path = |id: &str| format!("/conversations/{dm}/messages/{id}/acknowledgement");
    assert_eq!(
      app.put(&s.carol, &path(&plain), json!({})).await.status,
      StatusCode::BAD_REQUEST,
      "not urgent"
    );
    assert_eq!(
      app.put(&s.alice, &path(&message), json!({})).await.status,
      StatusCode::BAD_REQUEST,
      "own message"
    );
    assert_eq!(
      app.put(&eve, &path(&message), json!({})).await.status,
      StatusCode::NOT_FOUND,
      "stranger"
    );
    assert_eq!(
      app.get(&eve, &format!("{}s", path(&message))).await.status,
      StatusCode::NOT_FOUND
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn urgent_channel_messages_are_not_repeated_and_do_not_beat_do_not_disturb() {
    let recorder = Recorder::default();
    let Some(app) = start_urgent(&recorder).await else {
      return;
    };
    let s = scene(&app).await;
    let carol_id = user_id(&app, &s.carol).await;
    let mut body = message_body(1, &s.alice_device);
    body["is_urgent"] = json!(true);
    body["mentioned_user_ids"] = json!([carol_id]);
    app
      .post(&s.alice, &format!("/conversations/{}/messages", s.channel), body)
      .await;
    assert_eq!(recorder.wait_for(1).await.len(), 1);
    assert_eq!(
      run_due(&app.state, later(3600)).await.unwrap(),
      0,
      "channels have no repeats"
    );

    // In a DM, a recipient on do-not-disturb gets neither the first push nor the repeats.
    app.put(&s.carol, "/me/presence", json!({"manual": "dnd"})).await;
    urgent_dm(&app, &s, 2).await;
    assert_eq!(
      run_due(&app.state, later(3600)).await.unwrap(),
      1,
      "the attempt is counted"
    );
    assert_eq!(
      recorder.wait_for(1).await.len(),
      1,
      "no new push while do-not-disturb"
    );
    app.stop().await;
  }
}

mod web_push {
  use super::*;

  /// Headers and body of a received push.
  type Request = (HeaderMap, Vec<u8>);

  #[derive(Clone, Default)]
  struct FakeService {
    requests: Arc<Mutex<Vec<Request>>>,
    status: Arc<Mutex<u16>>,
  }

  async fn receive(State(service): State<FakeService>, headers: HeaderMap, body: Bytes) -> HttpStatus {
    service.requests.lock().unwrap().push((headers, body.to_vec()));
    HttpStatus::from_u16(*service.status.lock().unwrap()).unwrap()
  }

  impl FakeService {
    async fn start(status: u16) -> (Self, String) {
      let service = Self {
        requests: Arc::default(),
        status: Arc::new(Mutex::new(status)),
      };
      let app = Router::new()
        .route("/push/{id}", post(receive))
        .with_state(service.clone());
      let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
      let url = format!("http://{}/push/subscription-1", listener.local_addr().unwrap());
      tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
      });
      (service, url)
    }

    async fn wait_for_request(&self) -> (HeaderMap, Vec<u8>) {
      for _ in 0..60 {
        if let Some(request) = self.requests.lock().unwrap().first().cloned() {
          return request;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
      }
      panic!("the push service received nothing");
    }
  }

  const VAPID_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";

  async fn start_web_push() -> Option<TestApp> {
    let vapid = VapidKey::from_base64url(VAPID_PRIVATE).unwrap();
    let sender = WebPushSender::new(
      vapid,
      "mailto:ops@example.com".into(),
      Duration::from_secs(5),
      true,
    )
    .unwrap();
    let dispatcher = PushDispatcher::new(8).with_sender(&PushProvider::WebPush, Arc::new(sender));
    TestApp::start_with_push(|s| s.push.allow_private_endpoints = true, dispatcher).await
  }

  fn subscription(endpoint: &str, ua: &SecretKey, auth: &[u8]) -> String {
    json!({
      "endpoint": endpoint,
      "keys": {
        "p256dh": URL_SAFE_NO_PAD.encode(ua.public_key().to_encoded_point(false).as_bytes()),
        "auth": URL_SAFE_NO_PAD.encode(auth),
      }
    })
    .to_string()
  }

  async fn browser(app: &TestApp, user: &TestUser, endpoint: &str, ua: &SecretKey, auth: &[u8]) -> String {
    let device = register_device(app, user).await;
    let reply = app
      .put(
        user,
        &format!("/devices/{device}/push"),
        json!({"provider": "webpush", "token": subscription(endpoint, ua, auth), "push_key": push_key()}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    device
  }

  #[tokio::test]
  async fn a_message_reaches_the_browser_as_an_encrypted_vapid_signed_request() {
    let Some(app) = start_web_push().await else { return };
    let (service, endpoint) = FakeService::start(201).await;
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let alice_device = register_device(&app, &alice).await;
    let id = user_id(&app, &carol).await;
    app
      .patch(
        &carol,
        &format!("/conversations/{channel}/members/{id}"),
        json!({"notification_level": "all"}),
      )
      .await;
    let ua = SecretKey::random(&mut rand_core::OsRng);
    let auth = [4u8; 16];
    browser(&app, &carol, &endpoint, &ua, &auth).await;

    let message = send(&app, &alice, &channel, &alice_device, 1).await;
    let (headers, body) = service.wait_for_request().await;

    // Transport headers.
    assert_eq!(headers["content-encoding"], "aes128gcm");
    assert_eq!(headers["content-type"], "application/octet-stream");
    assert_eq!(headers["urgency"], "normal");
    assert_eq!(headers["ttl"], "3600");
    // VAPID: a token for the push service's origin, signed by the configured key.
    let vapid = VapidKey::from_base64url(VAPID_PRIVATE).unwrap();
    let authorization = headers["authorization"].to_str().unwrap();
    let (token, key) = authorization
      .strip_prefix("vapid t=")
      .unwrap()
      .split_once(", k=")
      .unwrap();
    assert_eq!(key, vapid.public_key_base64url());
    let point = URL_SAFE_NO_PAD.decode(key).unwrap();
    let decoding = jsonwebtoken::DecodingKey::from_ec_components(
      &URL_SAFE_NO_PAD.encode(&point[1..33]),
      &URL_SAFE_NO_PAD.encode(&point[33..65]),
    )
    .unwrap();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::ES256);
    validation.set_audience(&[endpoint.split("/push/").next().unwrap()]);
    let claims = jsonwebtoken::decode::<serde_json::Value>(token, &decoding, &validation)
      .unwrap()
      .claims;
    assert_eq!(claims["sub"], "mailto:ops@example.com");

    // Payload: only the browser can decrypt it, and inside is the sealed wake-up for the push key.
    let sealed = decrypt_web_push(&ua, &auth, &body).unwrap();
    let wake_up = open_wakeup(&PUSH_KEY_BYTES, &sealed).unwrap();
    assert_eq!(wake_up.maybe_message_id.unwrap().to_string(), message);
    assert_eq!(wake_up.conversation_id.to_string(), channel);
    assert!(
      !String::from_utf8_lossy(&body).contains(&channel),
      "nothing readable in transit"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn dead_subscriptions_are_forgotten_but_temporary_failures_are_not() {
    let Some(app) = start_web_push().await else { return };
    let (gone, gone_endpoint) = FakeService::start(410).await;
    let (flaky, flaky_endpoint) = FakeService::start(503).await;
    let (alice, carol, dave) = (owner(), member("carol"), member("dave"));
    let channel = channel_with(&app, &[&carol, &dave]).await;
    let alice_device = register_device(&app, &alice).await;
    for user in [&carol, &dave] {
      let id = user_id(&app, user).await;
      app
        .patch(
          user,
          &format!("/conversations/{channel}/members/{id}"),
          json!({"notification_level": "all"}),
        )
        .await;
    }
    let ua = SecretKey::random(&mut rand_core::OsRng);
    browser(&app, &carol, &gone_endpoint, &ua, &[1u8; 16]).await;
    browser(&app, &dave, &flaky_endpoint, &ua, &[1u8; 16]).await;

    send(&app, &alice, &channel, &alice_device, 1).await;
    gone.wait_for_request().await;
    flaky.wait_for_request().await;
    tokio::time::sleep(Duration::from_millis(400)).await;

    let carol_devices = app.get(&carol, "/devices").await;
    assert!(
      carol_devices.body["devices"][0].get("push_provider").is_none(),
      "410 removes the registration"
    );
    let dave_devices = app.get(&dave, "/devices").await;
    assert_eq!(
      dave_devices.body["devices"][0]["push_provider"], "webpush",
      "503 keeps it"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn web_push_devices_get_nothing_without_a_configured_sender() {
    let recorder = Recorder::default();
    let Some(app) =
      TestApp::start_with_push(|s| s.push.allow_private_endpoints = true, recorder.dispatcher()).await
    else {
      return;
    };
    let (service, endpoint) = FakeService::start(201).await;
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let alice_device = register_device(&app, &alice).await;
    let id = user_id(&app, &carol).await;
    app
      .patch(
        &carol,
        &format!("/conversations/{channel}/members/{id}"),
        json!({"notification_level": "all"}),
      )
      .await;
    let ua = SecretKey::random(&mut rand_core::OsRng);
    browser(&app, &carol, &endpoint, &ua, &[1u8; 16]).await;

    send(&app, &alice, &channel, &alice_device, 1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(service.requests.lock().unwrap().is_empty());
    app.stop().await;
  }
}

/// The real APNs, FCM and UnifiedPush senders against local fake push services, behind the whole
/// server: message in, encrypted wake-up out.
mod providers {
  use std::time::Duration;

  use teamcraft_server::push::{
    ApnsSender, ApnsSettings, FcmSender, FcmSettings, ServiceAccount, UnifiedPushSender,
  };

  use super::*;

  struct Services {
    apns: push_fakes::Fake,
    fcm: push_fakes::Fake,
    unified: push_fakes::Fake,
    unified_endpoint: String,
  }

  async fn start_providers() -> Option<(TestApp, Services)> {
    let (apns, apns_url) = push_fakes::apns().await;
    let (fcm, fcm_url) = push_fakes::fcm().await;
    let (unified, unified_endpoint) = push_fakes::unified_push().await;
    let timeout = Duration::from_secs(5);
    let apns_sender = ApnsSender::new(ApnsSettings {
      key_pem: push_fakes::APNS_KEY_PEM.to_owned().into(),
      key_id: push_fakes::APNS_KEY_ID.into(),
      team_id: push_fakes::APNS_TEAM_ID.into(),
      bundle_id: push_fakes::APNS_BUNDLE_ID.into(),
      base_url: apns_url,
      alert_text: "New message".into(),
      timeout,
    })
    .unwrap();
    let fcm_sender = FcmSender::new(FcmSettings {
      account: ServiceAccount::from_json(&push_fakes::fcm_service_account_json(&fcm_url)).unwrap(),
      base_url: fcm_url,
      timeout,
    })
    .unwrap();
    let dispatcher = PushDispatcher::new(8)
      .with_sender(&PushProvider::Apns, Arc::new(apns_sender))
      .with_sender(&PushProvider::Fcm, Arc::new(fcm_sender))
      .with_sender(
        &PushProvider::UnifiedPush,
        Arc::new(UnifiedPushSender::new(timeout, true).unwrap()),
      );
    let app = TestApp::start_with_push(
      |settings| {
        settings.push.allow_private_endpoints = true;
        settings.notifications.mobile_push_delay_secs = 0;
      },
      dispatcher,
    )
    .await?;
    Some((
      app,
      Services {
        apns,
        fcm,
        unified,
        unified_endpoint,
      },
    ))
  }

  async fn register(app: &TestApp, user: &TestUser, provider: &str, token: &str) -> String {
    let device = register_device(app, user).await;
    let reply = app
      .put(
        user,
        &format!("/devices/{device}/push"),
        json!({"provider": provider, "token": token, "push_key": push_key()}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    device
  }

  fn opened(sealed: &[u8]) -> PushWakeUp {
    open_wakeup(&PUSH_KEY_BYTES, sealed).unwrap()
  }

  const APNS_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

  #[tokio::test]
  async fn a_message_wakes_every_kind_of_phone_with_an_encrypted_wake_up() {
    let Some((app, services)) = start_providers().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let alice_device = register_device(&app, &alice).await;
    set_level(&app, &carol, &channel, "all").await;
    register(&app, &carol, "apns", APNS_TOKEN).await;
    register(&app, &carol, "fcm", "fcm-registration-token").await;
    register(&app, &carol, "unifiedpush", &services.unified_endpoint).await;

    let message = send(&app, &alice, &channel, &alice_device, 1).await;

    let expect = |wake_up: PushWakeUp| {
      assert_eq!(wake_up.kind, PushKind::Message);
      assert_eq!(wake_up.conversation_id.to_string(), channel);
      assert_eq!(wake_up.maybe_message_id.unwrap().to_string(), message);
    };
    let apns = services.apns.wait_for(1).await;
    assert_eq!(apns[0].header("apns-push-type"), "alert");
    expect(opened(&push_fakes::sealed_from(
      apns[0].json()["w"].as_str().unwrap(),
    )));

    let fcm = services.fcm.wait_for(1).await;
    expect(opened(&push_fakes::sealed_from(
      fcm[0].json()["message"]["data"]["w"].as_str().unwrap(),
    )));

    let unified = services.unified.wait_for(1).await;
    expect(opened(&unified[0].body));

    // The admin console reports what happened, from the real counters.
    let status = app.get(&admin(), "/admin/instance").await;
    let providers = status.body["push"].as_array().unwrap();
    for name in ["apns", "fcm", "unifiedpush"] {
      let entry = providers.iter().find(|entry| entry["provider"] == name).unwrap();
      assert_eq!(entry["state"], "active", "{name}");
      assert_eq!(entry["success_count"], 1, "{name}");
      assert_eq!(entry["failure_count"], 0, "{name}");
      assert!(entry["last_success_at"].is_number(), "{name}");
    }
    let web = providers
      .iter()
      .find(|entry| entry["provider"] == "webpush")
      .unwrap();
    assert_eq!(web["state"], "not_configured");
    app.stop().await;
  }

  #[tokio::test]
  async fn an_active_desktop_gets_no_phone_push_but_an_idle_one_does() {
    let Some((app, services)) = start_providers().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let alice_device = register_device(&app, &alice).await;
    set_level(&app, &carol, &channel, "all").await;
    register(&app, &carol, "fcm", "fcm-registration-token").await;

    // Carol is at her desktop: the phone stays quiet.
    let desktop = register_device(&app, &carol).await;
    let socket = ws::connect(&app, &carol, &desktop).await;
    send(&app, &alice, &channel, &alice_device, 1).await;
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(services.fcm.requests().is_empty(), "desktop is active");

    // She leaves the desktop: the next message reaches the phone.
    drop(socket);
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(&app, &alice, &channel, &alice_device, 2).await;
    assert_eq!(services.fcm.wait_for(1).await.len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_failing_push_service_shows_up_in_the_admin_console() {
    let Some((app, services)) = start_providers().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let alice_device = register_device(&app, &alice).await;
    set_level(&app, &carol, &channel, "all").await;
    register(&app, &carol, "fcm", "fcm-registration-token").await;
    services
      .fcm
      .set_reply(503, json!({"error": {"status": "UNAVAILABLE"}}));

    send(&app, &alice, &channel, &alice_device, 1).await;
    services.fcm.wait_for(1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let status = app.get(&admin(), "/admin/instance").await;
    let fcm = status.body["push"]
      .as_array()
      .unwrap()
      .iter()
      .find(|entry| entry["provider"] == "fcm")
      .unwrap()
      .clone();
    assert_eq!(fcm["state"], "failing");
    assert_eq!(fcm["failure_count"], 1);
    assert!(fcm["last_error"].as_str().unwrap().contains("503"));
    // A temporary failure keeps the registration.
    let devices = app.get(&carol, "/devices").await;
    assert!(
      devices.body["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|device| device["push_provider"] == "fcm")
    );
    app.stop().await;
  }
}
