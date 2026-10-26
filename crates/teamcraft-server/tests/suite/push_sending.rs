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
  PushDispatcher, PushError, PushMessage, PushSender, PushTarget, Urgency, Vapid, WebPushSender,
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
      json!({"provider": provider, "token": registration_token(provider), "push_key": push_key()}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
  device
}

/// A registration the server accepts: Web Push and UnifiedPush need a subscription with keys.
fn registration_token(provider: &str) -> &'static str {
  match provider {
    "webpush" | "unifiedpush" => {
      r#"{"endpoint":"https://push.example.com/up/abc","keys":{"p256dh":"BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4","auth":"BTBZMqHH6r4Tts7J_aSIgg"}}"#
    },
    _ => "t",
  }
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
    // Everything the user should see goes out at high priority (notifications.md, "Priority").
    assert_eq!(push.urgency, Urgency::High);
    assert!(push.wake_up.maybe_sent_at.is_some());
    // A channel message that doesn't mention carol is not part of the badge.
    assert_eq!(push.wake_up.maybe_badge, Some(0));
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

  /// Issue #756: of four quick messages only the first reached the phone. The server pushes every
  /// one of them, each naming its own message, so the app can add each line to the conversation's
  /// notification; nothing collapses or throttles them per conversation.
  #[tokio::test]
  async fn every_message_of_a_quick_burst_is_pushed() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let carol_id = user_id(&app, &s.carol).await;
    let dm = app
      .post(&s.alice, "/dms", json!({"user_id": carol_id}))
      .await
      .id();
    // An open desktop carol isn't at does not change that.
    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: false },
    )
    .await;

    let mut sent = Vec::new();
    for n in 1..=4 {
      sent.push(send(&app, &s.alice, &dm, &s.alice_device, n).await);
    }
    let pushes = recorder.wait_for(4).await;
    assert_eq!(pushes.len(), 4, "{pushes:?}");
    let mut pushed: Vec<String> = pushes
      .iter()
      .map(|push| {
        assert_eq!(push.device, s.carol_push_device);
        assert_eq!(push.wake_up.kind, PushKind::Mention);
        assert_eq!(push.urgency, Urgency::High);
        assert_eq!(push.wake_up.conversation_id.to_string(), dm);
        push.wake_up.maybe_message_id.unwrap().to_string()
      })
      .collect();
    pushed.sort();
    sent.sort();
    assert_eq!(pushed, sent);
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
  async fn deactivated_users_are_not_pushed_to_until_reactivated() {
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

    // The push registration was kept: after a reactivation pushes arrive again.
    app
      .post(
        &admin(),
        &format!("/admin/users/{carol_id}/reactivate"),
        json!({}),
      )
      .await;
    send(&app, &s.alice, &s.channel, &s.alice_device, 2).await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(
      pushes.iter().map(|p| p.device.clone()).collect::<Vec<_>>(),
      vec![s.carol_push_device.clone()]
    );
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
  async fn a_muted_space_silences_its_channels_unless_they_have_their_own_setting() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let spaces = app.get(&s.carol, "/spaces").await;
    let space = spaces.body["spaces"][0]["id"].as_str().unwrap().to_owned();
    let channel_path = format!("/conversations/{}/notification-settings", s.channel);

    let muted = app
      .put(
        &s.carol,
        &format!("/spaces/{space}/notification-settings"),
        json!({"level": "none"}),
      )
      .await;
    assert_eq!(muted.status, StatusCode::OK, "{}", muted.body);
    mention(&app, &s, 1).await;
    assert!(recorder.wait_for(0).await.is_empty(), "the space is muted");

    // A level chosen for the channel itself overrides the space.
    app.put(&s.carol, &channel_path, json!({"level": "all"})).await;
    mention(&app, &s, 2).await;
    assert_eq!(
      recorder.wait_for(1).await.len(),
      1,
      "the channel has its own level"
    );

    // Dropping the channel's own setting hands it back to the space.
    app
      .put(
        &s.carol,
        &channel_path,
        json!({"level": "all", "is_inherited": true}),
      )
      .await;
    mention(&app, &s, 3).await;
    assert_eq!(recorder.wait_for(1).await.len(), 1, "the space is muted again");
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
    let kinds: Vec<PushKind> = recorder
      .seen
      .lock()
      .unwrap()
      .iter()
      .map(|push| push.wake_up.kind.clone())
      .collect();
    // The read message was not pushed; the phone was told to remove the first one, now read too.
    assert_eq!(kinds, vec![PushKind::Mention, PushKind::Dismiss]);
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
      Vapid::new(vapid, "mailto:ops@example.com".into()),
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
    assert_eq!(headers["urgency"], "high");
    assert_eq!(headers["ttl"], "86400");
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
        Arc::new(UnifiedPushSender::new(None, timeout, true).unwrap()),
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
    // The Android app registers the distributor's endpoint with its Web Push keys.
    let unified_ua = SecretKey::random(&mut rand_core::OsRng);
    let unified_auth = [6u8; 16];
    let unified_subscription = json!({
      "endpoint": services.unified_endpoint,
      "keys": {
        "p256dh": URL_SAFE_NO_PAD.encode(unified_ua.public_key().to_encoded_point(false).as_bytes()),
        "auth": URL_SAFE_NO_PAD.encode(unified_auth),
      }
    })
    .to_string();
    register(&app, &carol, "unifiedpush", &unified_subscription).await;

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
    assert_eq!(unified[0].header("content-encoding"), "aes128gcm");
    expect(opened(
      &decrypt_web_push(&unified_ua, &unified_auth, &unified[0].body).unwrap(),
    ));

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

/// The whole server with APNs and FCM behind a (fake) push gateway: message in, sealed wake-up out,
/// and what the gateway says about a token decides what the server keeps.
mod via_gateway {
  use super::*;
  use crate::push_gateway::Stack;

  async fn start() -> Option<(TestApp, Stack)> {
    let mut stack = Stack::start().await;
    // The app takes the dispatcher; the rest of the stack stays for the assertions.
    let dispatcher = std::mem::replace(&mut stack.dispatcher, PushDispatcher::new(1));
    let app = TestApp::start_with_push(
      |settings| settings.notifications.mobile_push_delay_secs = 0,
      dispatcher,
    )
    .await?;
    Some((app, stack))
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

  fn has_push(devices: &serde_json::Value, device: &str) -> bool {
    devices["devices"]
      .as_array()
      .unwrap()
      .iter()
      .any(|entry| entry["id"] == device && entry["push_provider"].is_string())
  }

  const APNS_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

  #[tokio::test]
  async fn a_message_wakes_ios_and_android_through_the_gateway() {
    let Some((app, stack)) = start().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let alice_device = register_device(&app, &alice).await;
    set_level(&app, &carol, &channel, "all").await;
    register(&app, &carol, "apns", &stack.sealed_token("apns", APNS_TOKEN)).await;
    register(
      &app,
      &carol,
      "fcm",
      &stack.sealed_token("fcm", "fcm-registration-token"),
    )
    .await;

    let message = send(&app, &alice, &channel, &alice_device, 1).await;

    let pushes = stack.fake.wait_for(2).await;
    assert_eq!(pushes.len(), 2);
    let providers: Vec<_> = pushes
      .iter()
      .map(|push| push.request.provider.as_str().to_owned())
      .collect();
    assert!(providers.contains(&"apns".to_owned()) && providers.contains(&"fcm".to_owned()));
    for push in &pushes {
      let wake_up = open_wakeup(&PUSH_KEY_BYTES, push.request.sealed.as_slice()).unwrap();
      assert_eq!(wake_up.kind, PushKind::Message);
      assert_eq!(wake_up.conversation_id.to_string(), channel);
      assert_eq!(wake_up.maybe_message_id.unwrap().to_string(), message);
    }
    // The admin console sees APNs and FCM as active, like with direct credentials.
    let status = app.get(&admin(), "/admin/instance").await;
    for name in ["apns", "fcm"] {
      let entry = status.body["push"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["provider"] == name)
        .unwrap()
        .clone();
      assert_eq!(entry["state"], "active", "{name}");
      assert_eq!(entry["success_count"], 1, "{name}");
    }
    app.stop().await;
  }

  #[tokio::test]
  async fn only_a_token_the_gateway_reports_dead_is_forgotten() {
    let Some((app, stack)) = start().await else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let alice_device = register_device(&app, &alice).await;
    set_level(&app, &carol, &channel, "all").await;
    let phone = register(&app, &carol, "apns", &stack.sealed_token("apns", APNS_TOKEN)).await;

    // The gateway blocks this server: the push fails, the registration stays.
    stack.fake.set_reply(Some((
      403,
      json!({"error_code": "server_blocked", "message": "blocked"}),
    )));
    send(&app, &alice, &channel, &alice_device, 1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(stack.fake.pushes().is_empty());
    assert!(has_push(&app.get(&carol, "/devices").await.body, &phone));

    // The push service says the token is dead: the server drops the registration.
    stack.fake.set_reply(Some((
      410,
      json!({"error_code": "token_gone", "message": "dead"}),
    )));
    send(&app, &alice, &channel, &alice_device, 2).await;
    for _ in 0..50 {
      if !has_push(&app.get(&carol, "/devices").await.body, &phone) {
        break;
      }
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!has_push(&app.get(&carol, "/devices").await.body, &phone));
    app.stop().await;
  }

  #[tokio::test]
  async fn apps_learn_how_to_seal_and_the_server_takes_only_sealed_tokens() {
    let Some((app, stack)) = start().await else {
      return;
    };
    let carol = member("carol");
    let device = register_device(&app, &carol).await;

    let info = app.get(&carol, "/push/gateway").await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.body);
    let gateway = &info.body["gateway"];
    assert_eq!(gateway["server_id"], stack.client.server_id());
    assert_eq!(gateway["url"], stack.client.url());
    assert_eq!(gateway["providers"], json!(["apns", "fcm"]));

    let put = |token: String| {
      let path = format!("/devices/{device}/push");
      let body = json!({"provider": "apns", "token": token, "push_key": push_key()});
      let (app, carol) = (&app, &carol);
      async move { app.put(carol, &path, body).await }
    };
    // A raw token would let any server that learns it push to the device: refused.
    assert_eq!(put(APNS_TOKEN.into()).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(
      put(stack.sealed_token("apns", APNS_TOKEN)).await.status,
      StatusCode::NO_CONTENT
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn the_transition_setting_lets_old_apps_register_raw_tokens() {
    let stack = Stack::start_with(true).await;
    let mut stack = stack;
    let dispatcher = std::mem::replace(&mut stack.dispatcher, PushDispatcher::new(1));
    let Some(app) = TestApp::start_with_push(|_| {}, dispatcher).await else {
      return;
    };
    let carol = member("carol");
    let device = register_device(&app, &carol).await;

    let reply = app
      .put(
        &carol,
        &format!("/devices/{device}/push"),
        json!({"provider": "apns", "token": APNS_TOKEN, "push_key": push_key()}),
      )
      .await;

    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn without_a_gateway_apps_get_no_binding_and_sealed_tokens_are_refused() {
    let Some(app) = TestApp::start().await else {
      return;
    };
    let carol = member("carol");
    let device = register_device(&app, &carol).await;

    let info = app.get(&carol, "/push/gateway").await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.body);
    assert!(info.body.get("gateway").is_none());
    let reply = app
      .put(
        &carol,
        &format!("/devices/{device}/push"),
        json!({"provider": "apns", "token": "tcgw1:AAAA", "push_key": push_key()}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    app.stop().await;
  }
}

/// Sends a gateway frame from a connected test client.
async fn send_frame(socket: &mut ws::Socket, message: &GatewayClientMessage) {
  socket
    .send(Message::Text(serde_json::to_string(message).unwrap().into()))
    .await
    .unwrap();
  // The server handles frames in order; a heartbeat round trip shows this one was seen.
  socket
    .send(Message::Text(
      serde_json::to_string(&GatewayClientMessage::Heartbeat)
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();
  while let Ok(Some(Ok(Message::Text(text)))) =
    tokio::time::timeout(Duration::from_secs(5), socket.next()).await
  {
    if text.as_str().contains("heartbeat_ack") {
      break;
    }
  }
}

fn kinds(pushes: &[Recorded]) -> Vec<PushKind> {
  pushes.iter().map(|push| push.wake_up.kind.clone()).collect()
}

async fn mention_carol(app: &TestApp, s: &Scene, n: u8) -> String {
  let id = user_id(app, &s.carol).await;
  let mut body = message_body(n, &s.alice_device);
  body["mentioned_user_ids"] = json!([id]);
  let reply = app
    .post(&s.alice, &format!("/conversations/{}/messages", s.channel), body)
    .await;
  assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  reply.id()
}

/// Desktop attention (notifications.md, "Attention").
mod attention {
  use super::*;

  async fn start_with_grace(recorder: &Recorder, grace_secs: u64) -> Option<TestApp> {
    TestApp::start_with_push(
      |settings| settings.notifications.mobile_push_delay_secs = grace_secs,
      recorder.dispatcher(),
    )
    .await
  }

  #[tokio::test]
  async fn an_open_desktop_the_user_does_not_look_at_never_delays_the_phone() {
    let recorder = Recorder::default();
    let Some(app) = start_with_grace(&recorder, 30).await else {
      return;
    };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: false },
    )
    .await;

    mention_carol(&app, &s, 1).await;
    assert_eq!(kinds(&recorder.wait_for(1).await), vec![PushKind::Mention]);
    app.stop().await;
  }

  /// Issue #756: "push to the phone only when I'm not at the desktop". Input on the computer counts
  /// although the app's window is in the background (or the webview reports its focus wrongly).
  #[tokio::test]
  async fn working_at_the_desktop_holds_the_phone_back_even_with_the_window_behind() {
    let recorder = Recorder::default();
    let Some(app) = start_with_grace(&recorder, 1).await else {
      return;
    };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: false },
    )
    .await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Active,
      },
    )
    .await;

    mention_carol(&app, &s, 1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
      recorder.seen.lock().unwrap().is_empty(),
      "held while carol works at the desktop"
    );
    // Still unread after the grace delay: the phone gets it.
    assert_eq!(kinds(&recorder.wait_for(1).await), vec![PushKind::Mention]);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_desktop_going_idle_releases_a_held_push_at_once() {
    let recorder = Recorder::default();
    let Some(app) = start_with_grace(&recorder, 30).await else {
      return;
    };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: false },
    )
    .await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Active,
      },
    )
    .await;
    mention_carol(&app, &s, 1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(recorder.seen.lock().unwrap().is_empty(), "held");

    // The gateway takes one activity change a second; then the screen locks.
    tokio::time::sleep(Duration::from_millis(800)).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Idle,
      },
    )
    .await;
    assert_eq!(kinds(&recorder.wait_for(1).await), vec![PushKind::Mention]);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_desktop_without_recent_activity_never_delays_the_phone() {
    let recorder = Recorder::default();
    let Some(app) = TestApp::start_with_push(
      |settings| {
        settings.notifications.mobile_push_delay_secs = 30;
        settings.notifications.desktop_activity_secs = 1;
      },
      recorder.dispatcher(),
    )
    .await
    else {
      return;
    };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: false },
    )
    .await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::SetActivity {
        activity: DeviceActivity::Active,
      },
    )
    .await;
    // Carol left the desktop without locking it: once the activity window passed, the phone is
    // pushed at once.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    mention_carol(&app, &s, 1).await;
    assert_eq!(kinds(&recorder.wait_for(1).await), vec![PushKind::Mention]);
    app.stop().await;
  }

  #[tokio::test]
  async fn looking_away_releases_a_held_push_before_the_grace_delay_ends() {
    let recorder = Recorder::default();
    let Some(app) = start_with_grace(&recorder, 30).await else {
      return;
    };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: true },
    )
    .await;

    mention_carol(&app, &s, 1).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
      recorder.seen.lock().unwrap().is_empty(),
      "held while carol looks at the desktop"
    );
    // The screen locks: the phone gets the push now, not after 30 seconds.
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: false },
    )
    .await;
    assert_eq!(kinds(&recorder.wait_for(1).await), vec![PushKind::Mention]);
    app.stop().await;
  }

  #[tokio::test]
  async fn users_who_turn_attention_off_are_held_by_presence_only() {
    let recorder = Recorder::default();
    let Some(app) = start_with_grace(&recorder, 1).await else {
      return;
    };
    let s = scene(&app).await;
    let reply = app
      .put(
        &s.carol,
        "/me/notification-settings",
        json!({"auto_follow_mentioned_threads": true, "push_mode": "away_only", "send_read_receipts": true,
               "uses_desktop_attention": false}),
      )
      .await;
    assert_eq!(reply.body["uses_desktop_attention"], false, "{}", reply.body);
    // A later write that doesn't name the setting keeps it.
    let kept = app
      .put(
        &s.carol,
        "/me/notification-settings",
        json!({"auto_follow_mentioned_threads": true, "push_mode": "away_only", "send_read_receipts": true}),
      )
      .await;
    assert_eq!(kept.body["uses_desktop_attention"], false, "{}", kept.body);

    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    // The report is ignored: the active desktop holds the push as presence would.
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: false },
    )
    .await;
    mention_carol(&app, &s, 1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
      recorder.seen.lock().unwrap().is_empty(),
      "held by the active desktop"
    );
    assert_eq!(kinds(&recorder.wait_for(1).await), vec![PushKind::Mention]);
    app.stop().await;
  }

  #[tokio::test]
  async fn an_instance_without_attention_says_so_and_ignores_reports() {
    let recorder = Recorder::default();
    let Some(app) = TestApp::start_with_push(
      |settings| {
        settings.notifications.mobile_push_delay_secs = 1;
        settings.notifications.is_desktop_attention_enabled = false;
      },
      recorder.dispatcher(),
    )
    .await
    else {
      return;
    };
    let s = scene(&app).await;
    let settings = app.get(&s.carol, "/me/notification-settings").await;
    assert_eq!(
      settings.body["uses_desktop_attention"], false,
      "{}",
      settings.body
    );
    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: false },
    )
    .await;
    mention_carol(&app, &s, 1).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
      recorder.seen.lock().unwrap().is_empty(),
      "held by the active desktop"
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn closing_the_desktop_releases_a_held_push() {
    let recorder = Recorder::default();
    let Some(app) = start_with_grace(&recorder, 30).await else {
      return;
    };
    let s = scene(&app).await;
    let desktop = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &desktop).await;
    send_frame(
      &mut socket,
      &GatewayClientMessage::Attention { is_attentive: true },
    )
    .await;

    mention_carol(&app, &s, 1).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(socket);
    assert_eq!(kinds(&recorder.wait_for(1).await), vec![PushKind::Mention]);
    app.stop().await;
  }
}

/// Reading on one device removes the notification on the others.
mod read_elsewhere {
  use super::*;

  #[tokio::test]
  async fn reading_the_conversation_withdraws_the_phones_notification_with_the_new_badge() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let message = mention_carol(&app, &s, 1).await;
    let first = recorder.wait_for(1).await;
    assert_eq!(kinds(&first), vec![PushKind::Mention]);
    assert_eq!(
      first[0].wake_up.maybe_badge,
      Some(1),
      "one conversation with an unread mention"
    );

    let read = app
      .put(
        &s.carol,
        &format!("/conversations/{}/read-position", s.channel),
        json!({"message_id": message}),
      )
      .await;
    assert_eq!(read.status, StatusCode::NO_CONTENT, "{}", read.body);
    let pushes = recorder.wait_for(2).await;
    assert_eq!(kinds(&pushes), vec![PushKind::Mention, PushKind::Dismiss]);
    let dismiss = &pushes[1];
    assert_eq!(dismiss.device, s.carol_push_device);
    assert_eq!(dismiss.wake_up.conversation_id.to_string(), s.channel);
    assert_eq!(dismiss.wake_up.maybe_message_id.unwrap().to_string(), message);
    assert_eq!(dismiss.wake_up.maybe_thread_id, None);
    assert_eq!(dismiss.wake_up.maybe_badge, Some(0));
    // Withdrawals are silent: they never need to wake a dozing phone.
    assert_eq!(dismiss.urgency, Urgency::Normal);

    // Reading again changes nothing on the phone.
    app
      .put(
        &s.carol,
        &format!("/conversations/{}/read-position", s.channel),
        json!({"message_id": message}),
      )
      .await;
    assert_eq!(recorder.wait_for(3).await.len(), 2);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_notification_for_a_newer_message_stays_until_that_is_read() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let first = mention_carol(&app, &s, 1).await;
    mention_carol(&app, &s, 2).await;
    recorder.wait_for(2).await;

    app
      .put(
        &s.carol,
        &format!("/conversations/{}/read-position", s.channel),
        json!({"message_id": first}),
      )
      .await;
    assert_eq!(
      kinds(&recorder.wait_for(3).await),
      vec![PushKind::Mention, PushKind::Mention]
    );
    app.stop().await;
  }

  #[tokio::test]
  async fn reading_a_thread_withdraws_only_that_threads_notification() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let carol_id = user_id(&app, &s.carol).await;
    let root = send_in(&app, &s.carol, &s.channel, &s.carol_push_device, 1, None, &[]).await;
    let reply = send_in(
      &app,
      &s.alice,
      &s.channel,
      &s.alice_device,
      2,
      Some(&root),
      &[carol_id.as_str()],
    )
    .await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(kinds(&pushes), vec![PushKind::Mention]);
    assert_eq!(pushes[0].wake_up.maybe_thread_id.unwrap().to_string(), root);

    let read = app
      .put(
        &s.carol,
        &format!("/conversations/{}/threads/{root}/read", s.channel),
        json!({"message_id": reply}),
      )
      .await;
    assert_eq!(read.status, StatusCode::NO_CONTENT, "{}", read.body);
    let pushes = recorder.wait_for(2).await;
    assert_eq!(kinds(&pushes), vec![PushKind::Mention, PushKind::Dismiss]);
    assert_eq!(pushes[1].wake_up.maybe_thread_id.unwrap().to_string(), root);
    app.stop().await;
  }
}

/// DND overrides: allowed people and urgent messages (notifications.md, "DND").
mod dnd_overrides {
  use super::*;

  async fn set_dnd(
    app: &TestApp,
    user: &TestUser,
    allows_urgent: bool,
    time_zone: &str,
  ) -> reqwest::StatusCode {
    let reply = app
      .put(
        user,
        "/me/notification-settings",
        json!({
          "auto_follow_mentioned_threads": true,
          "push_mode": "away_only",
          "send_read_receipts": true,
          "dnd_schedule": {
            "utc_offset_minutes": 0,
            "time_zone": time_zone,
            "windows": [],
            "allow_user_ids": [],
            "allows_urgent": allows_urgent,
            // The scene is a channel; DMs are the default scope.
            "urgent_scope": "everywhere",
          },
        }),
      )
      .await;
    reply.status
  }

  async fn urgent_mention(app: &TestApp, s: &Scene, n: u8, is_urgent: bool) {
    let id = user_id(app, &s.carol).await;
    let mut body = message_body(n, &s.alice_device);
    body["mentioned_user_ids"] = json!([id]);
    body["is_urgent"] = json!(is_urgent);
    let reply = app
      .post(&s.alice, &format!("/conversations/{}/messages", s.channel), body)
      .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.body);
  }

  #[tokio::test]
  async fn urgent_messages_break_through_manual_dnd_only_when_allowed() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let dnd = app.put(&s.carol, "/me/presence", json!({"manual": "dnd"})).await;
    assert!(dnd.status.is_success(), "{}", dnd.body);

    assert_eq!(
      set_dnd(&app, &s.carol, false, "Europe/Berlin").await,
      StatusCode::OK
    );
    urgent_mention(&app, &s, 1, true).await;
    assert!(
      recorder.wait_for(0).await.is_empty(),
      "urgent does not break through by default"
    );

    assert_eq!(
      set_dnd(&app, &s.carol, true, "Europe/Berlin").await,
      StatusCode::OK
    );
    urgent_mention(&app, &s, 2, false).await;
    urgent_mention(&app, &s, 3, true).await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(pushes.len(), 1, "{pushes:?}");
    assert!(pushes[0].wake_up.is_urgent);
    app.stop().await;
  }

  #[tokio::test]
  async fn urgent_messages_in_channels_break_through_only_where_the_user_widened_it() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    app.put(&s.carol, "/me/presence", json!({"manual": "dnd"})).await;
    let put = |scope: &'static str| {
      json!({
        "auto_follow_mentioned_threads": true,
        "push_mode": "away_only",
        "send_read_receipts": true,
        "dnd_schedule": {"utc_offset_minutes": 0, "windows": [], "allow_user_ids": [],
                         "allows_urgent": true, "urgent_scope": scope},
      })
    };
    assert_eq!(
      app
        .put(&s.carol, "/me/notification-settings", put("direct"))
        .await
        .status,
      StatusCode::OK
    );
    urgent_mention(&app, &s, 1, true).await;
    assert!(recorder.wait_for(0).await.is_empty(), "a channel is not a DM");
    assert_eq!(
      app
        .put(&s.carol, "/me/notification-settings", put("everywhere"))
        .await
        .status,
      StatusCode::OK
    );
    urgent_mention(&app, &s, 2, true).await;
    assert_eq!(recorder.wait_for(1).await.len(), 1);
    app.stop().await;
  }

  #[tokio::test]
  async fn unknown_time_zones_are_refused_and_other_devices_hear_about_changes() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let other_device = register_device(&app, &s.carol).await;
    let mut socket = ws::connect(&app, &s.carol, &other_device).await;

    assert_eq!(
      set_dnd(&app, &s.carol, false, "Mars/Olympus").await,
      StatusCode::BAD_REQUEST
    );
    assert_eq!(
      set_dnd(&app, &s.carol, false, "America/New_York").await,
      StatusCode::OK
    );
    let events = ws::events(&mut socket, Duration::from_millis(500)).await;
    assert!(
      events
        .iter()
        .any(|event| matches!(event, teamcraft_proto::GatewayEvent::NotificationSettingsUpdate)),
      "{events:?}"
    );
    let stored = app.get(&s.carol, "/me/notification-settings").await;
    assert_eq!(stored.body["dnd_schedule"]["time_zone"], "America/New_York");
    app.stop().await;
  }
}

/// Rings reach phones without a live connection and stop when the ring ends.
mod rings {
  use std::path::PathBuf;

  use teamcraft_server::config::CallsConfig;

  use super::*;

  /// Throwaway Ed25519 key used only by these tests. It protects nothing.
  const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
  const SFU_TOKEN: &str = "sfu-registration-token-for-tests";

  #[tokio::test]
  async fn a_ring_wakes_the_phone_and_answering_stops_it() {
    let recorder = Recorder::default();
    let Some(app) =
      TestApp::start_with_calls_and_push(&calls_config(), SIGNING_KEY_PEM, recorder.dispatcher()).await
    else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let dm = app.post(&alice, "/dms", json!({"user_id": carol_id})).await;
    assert!(dm.status.is_success(), "{}", dm.body);
    let dm = dm.id();
    let alice_device = register_device(&app, &alice).await;
    let carol_phone = register_push_device(&app, &carol, "fcm").await;
    let carol_desktop = register_device(&app, &carol).await;
    let alice_id = user_id(&app, &alice).await;

    let call = join_token(&app, &alice, &dm, &alice_device).await;
    report(&app, "joined", &call, &alice_id, &alice_device).await;
    let pushes = recorder.wait_for(1).await;
    assert_eq!(kinds(&pushes), vec![PushKind::Call]);
    assert_eq!(pushes[0].device, carol_phone);
    assert_eq!(pushes[0].wake_up.maybe_call_id.unwrap().to_string(), call);
    assert_eq!(pushes[0].wake_up.maybe_sender_id.unwrap().to_string(), alice_id);
    assert_eq!(pushes[0].urgency, Urgency::High);

    // Carol answers on her desktop: the phone stops ringing.
    report(&app, "joined", &call, &carol_id, &carol_desktop).await;
    let pushes = recorder.wait_for(2).await;
    assert_eq!(kinds(&pushes), vec![PushKind::Call, PushKind::Dismiss]);
    assert_eq!(pushes[1].wake_up.maybe_call_id.unwrap().to_string(), call);
    app.stop().await;
  }

  #[tokio::test]
  async fn a_caller_ringing_too_often_reaches_open_apps_only() {
    let recorder = Recorder::default();
    let Some(app) =
      TestApp::start_with_calls_and_push(&calls_config(), SIGNING_KEY_PEM, recorder.dispatcher()).await
    else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let dm = app.post(&alice, "/dms", json!({"user_id": carol_id})).await.id();
    let alice_device = register_device(&app, &alice).await;
    register_push_device(&app, &carol, "fcm").await;
    let alice_id = user_id(&app, &alice).await;
    // Ring, hang up, ring again: the default allows 10 a minute.
    for _ in 0..11 {
      let call = join_token(&app, &alice, &dm, &alice_device).await;
      report(&app, "joined", &call, &alice_id, &alice_device).await;
      report(&app, "left", &call, &alice_id, &alice_device).await;
    }
    let rings = recorder
      .wait_for(30)
      .await
      .iter()
      .filter(|push| push.wake_up.kind == PushKind::Call)
      .count();
    assert_eq!(rings, 10);
    app.stop().await;
  }

  #[tokio::test]
  async fn do_not_disturb_keeps_rings_away() {
    let recorder = Recorder::default();
    let Some(app) =
      TestApp::start_with_calls_and_push(&calls_config(), SIGNING_KEY_PEM, recorder.dispatcher()).await
    else {
      return;
    };
    let (alice, carol) = (owner(), member("carol"));
    let carol_id = user_id(&app, &carol).await;
    let dm = app.post(&alice, "/dms", json!({"user_id": carol_id})).await.id();
    let alice_device = register_device(&app, &alice).await;
    register_push_device(&app, &carol, "fcm").await;
    app.put(&carol, "/me/presence", json!({"manual": "dnd"})).await;

    let call = join_token(&app, &alice, &dm, &alice_device).await;
    report(&app, "joined", &call, &user_id(&app, &alice).await, &alice_device).await;
    assert!(recorder.wait_for(0).await.is_empty());
    app.stop().await;
  }

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
      maybe_sfu_certificate_sha256: None,
    }
  }

  async fn join_token(app: &TestApp, user: &TestUser, conversation: &str, device: &str) -> String {
    let reply = app
      .post(
        user,
        &format!("/conversations/{conversation}/call/join-token"),
        json!({ "device_id": device }),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    reply.body["call_id"].as_str().unwrap().to_owned()
  }

  async fn report(app: &TestApp, action: &str, call_id: &str, user_id: &str, device_id: &str) {
    let status = app
      .http
      .post(format!(
        "{}/api/internal/v1/calls/participants/{action}",
        app.base_url
      ))
      .bearer_auth(SFU_TOKEN)
      .json(&json!({ "call_id": call_id, "user_id": user_id, "device_id": device_id }))
      .send()
      .await
      .unwrap()
      .status();
    assert_eq!(status, StatusCode::NO_CONTENT);
  }
}

/// Test pushes and delivery counts (notifications.md, "Diagnostics API").
mod diagnostics {
  use super::*;

  #[tokio::test]
  async fn a_test_push_reaches_the_own_device_and_is_rate_limited() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;

    let reply = app
      .post(
        &s.carol,
        &format!("/devices/{}/push/test", s.carol_push_device),
        json!({}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["outcome"], "sent");
    assert_eq!(reply.body["provider"], "fcm");
    let pushes = recorder.wait_for(1).await;
    assert_eq!(kinds(&pushes), vec![PushKind::Test]);
    assert_eq!(pushes[0].device, s.carol_push_device);

    let again = app
      .post(
        &s.carol,
        &format!("/devices/{}/push/test", s.carol_push_device),
        json!({}),
      )
      .await;
    assert_eq!(again.status, StatusCode::TOO_MANY_REQUESTS);
    app.stop().await;
  }

  #[tokio::test]
  async fn test_pushes_go_only_to_own_registered_devices() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    let foreign = app
      .post(
        &s.alice,
        &format!("/devices/{}/push/test", s.carol_push_device),
        json!({}),
      )
      .await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    let unregistered = register_device(&app, &s.carol).await;
    let reply = app
      .post(&s.carol, &format!("/devices/{unregistered}/push/test"), json!({}))
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["outcome"], "not_registered");
    assert!(recorder.wait_for(0).await.is_empty());
    app.stop().await;
  }

  #[tokio::test]
  async fn admins_see_delivery_counts_and_nothing_else() {
    let recorder = Recorder::default();
    let Some(app) = start(&recorder).await else { return };
    let s = scene(&app).await;
    mention_carol(&app, &s, 1).await;
    recorder.wait_for(1).await;
    let carol_id = user_id(&app, &s.carol).await;

    let health = app
      .get(&s.alice, &format!("/admin/users/{carol_id}/push-health"))
      .await;
    assert_eq!(health.status, StatusCode::OK, "{}", health.body);
    let device = health.body["devices"]
      .as_array()
      .unwrap()
      .iter()
      .find(|device| device["device_id"] == s.carol_push_device.as_str())
      .unwrap()
      .clone();
    assert_eq!(device["provider"], "fcm");
    assert_eq!(device["counts"]["sent"], 1);
    assert_eq!(device["counts"]["failed"], 0);
    let text = health.body.to_string();
    assert!(
      !text.contains(&s.channel),
      "no conversation in the counts: {text}"
    );

    let denied = app
      .get(&s.carol, &format!("/admin/users/{carol_id}/push-health"))
      .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    app.stop().await;
  }
}
