//! Push sending: routing rules, encrypted wake-ups and Web Push against a local fake push service.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode as HttpStatus};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
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
