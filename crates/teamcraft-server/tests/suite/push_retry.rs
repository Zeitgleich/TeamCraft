//! The durable push retry queue (notifications.md, "Mobile push delivery") against a fake push
//! gateway: wake-ups that fail for a temporary reason survive a killed server and arrive once
//! after the restart, within their TTL; `Retry-After`, the TTL and the per-device cap bound them.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::fake_gateway::FakeGateway;
use crate::common::*;

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use reqwest::StatusCode;
use serde_json::json;
use teamcraft_crypto::push_gateway::seal_bound_token;
use teamcraft_proto::{BOUND_TOKEN_PREFIX, DeviceId, PushKind, PushProvider};
use teamcraft_server::push::crypto::open_wakeup;
use teamcraft_server::push::{GatewayClient, GatewaySettings, PushDispatcher, parse_gateway_key};
use teamcraft_server::state::Settings;
use teamcraft_server::testing::{TestApp, TestUser};

const PUSH_KEY: [u8; 32] = [9; 32];
/// The server's gateway key, the same before and after a restart (it lives in a file).
const SERVER_KEY: [u8; 32] = [3; 32];
const FCM_TOKEN: &str = "fcm-registration-token";

#[tokio::test]
async fn a_wake_up_survives_kill_9_during_an_outage_and_arrives_once_after_the_restart() {
  let Some(mut scene) = Scene::start(|_| {}).await else {
    return;
  };
  // The push service is down: the first attempt fails and the wake-up goes into the queue.
  scene.fake.set_reply(Some(outage()));
  let message = send(&scene.app, &scene.alice, &scene.channel, &scene.alice_device, 1).await;
  scene.fake.wait_for_attempts(2).await;
  wait_until(|| async { scene.queued().await == 1 }).await;
  assert_eq!(scene.queued().await, 1);

  // The next retry hangs at the gateway, and the server is killed while it is in flight.
  scene.fake.set_hanging(true);
  let attempts = scene.fake.push_attempts();
  scene.fake.wait_for_attempts(attempts + 1).await;
  scene.app.kill_and_restart(scene.dispatcher()).await;

  // After the restart the push service is back: the wake-up arrives, once, and leaves the queue.
  scene.fake.set_hanging(false);
  scene.fake.set_reply(None);
  let pushes = scene.fake.wait_for(1).await;
  tokio::time::sleep(Duration::from_millis(500)).await;
  let pushes_later = scene.fake.pushes();
  assert_eq!(pushes.len(), 1);
  assert_eq!(pushes_later.len(), 1, "no duplicate after the restart");
  let wake_up = open_wakeup(&PUSH_KEY, pushes_later[0].request.sealed.as_slice()).unwrap();
  assert_eq!(wake_up.kind, PushKind::Message);
  assert_eq!(wake_up.maybe_message_id.unwrap().to_string(), message);
  assert_eq!(pushes_later[0].maybe_bound.as_ref().unwrap().token, FCM_TOKEN);
  assert_eq!(scene.queued().await, 0);
  scene.app.stop().await;
}

#[tokio::test]
async fn while_the_provider_is_failing_new_wake_ups_are_queued_before_the_attempt() {
  let Some(mut scene) = Scene::start(|_| {}).await else {
    return;
  };
  scene.fake.set_reply(Some(outage()));
  send(&scene.app, &scene.alice, &scene.channel, &scene.alice_device, 1).await;
  scene.fake.wait_for_attempts(1).await;
  // The provider now counts as failing; the next wake-up is written down before any attempt, so
  // a crash right after the message was sent still loses nothing.
  scene.fake.set_hanging(true);
  send(&scene.app, &scene.alice, &scene.channel, &scene.alice_device, 2).await;
  wait_until(|| async { scene.queued().await == 2 }).await;
  scene.app.kill_and_restart(scene.dispatcher()).await;

  scene.fake.set_hanging(false);
  scene.fake.set_reply(None);
  let pushes = scene.fake.wait_for(2).await;
  tokio::time::sleep(Duration::from_millis(500)).await;
  assert_eq!(pushes.len(), 2);
  assert_eq!(scene.fake.pushes().len(), 2, "each wake-up once");
  scene.app.stop().await;
}

#[tokio::test]
async fn retry_after_from_the_gateway_is_honoured() {
  let Some(scene) = Scene::start(|_| {}).await else {
    return;
  };
  scene.fake.set_reply(Some((
    502,
    json!({"error_code": "provider_unavailable", "message": "down", "retry_after_secs": 3600}),
  )));
  send(&scene.app, &scene.alice, &scene.channel, &scene.alice_device, 1).await;
  scene.fake.wait_for_attempts(1).await;
  // The backoff alone would have tried again within a few hundred milliseconds.
  tokio::time::sleep(Duration::from_millis(800)).await;
  assert_eq!(scene.fake.push_attempts(), 1);
  assert_eq!(scene.queued().await, 1, "kept for later, not dropped");
  scene.app.stop().await;
}

#[tokio::test]
async fn nothing_is_sent_after_the_ttl() {
  let Some(scene) = Scene::start(|settings| {
    settings.push.ttls.message = Duration::from_millis(600);
  })
  .await
  else {
    return;
  };
  scene.fake.set_reply(Some(outage()));
  send(&scene.app, &scene.alice, &scene.channel, &scene.alice_device, 1).await;
  scene.fake.wait_for_attempts(2).await;
  tokio::time::sleep(Duration::from_millis(800)).await;
  scene.fake.set_reply(None);
  tokio::time::sleep(Duration::from_millis(500)).await;
  assert!(
    scene.fake.pushes().is_empty(),
    "a stale wake-up is never delivered"
  );
  scene.app.stop().await;
}

#[tokio::test]
async fn a_device_holds_only_so_many_waiting_wake_ups() {
  let Some(scene) = Scene::start(|settings| {
    settings.push.retry.max_queued_per_device = 2;
    // Long waits: the queue only fills.
    settings.push.retry.initial_delay = Duration::from_secs(60);
  })
  .await
  else {
    return;
  };
  scene.fake.set_reply(Some(outage()));
  for n in 1..=4 {
    send(&scene.app, &scene.alice, &scene.channel, &scene.alice_device, n).await;
  }
  scene.fake.wait_for_attempts(1).await;
  tokio::time::sleep(Duration::from_millis(500)).await;
  assert_eq!(scene.queued().await, 2);
  scene.app.stop().await;
}

#[tokio::test]
async fn a_revoked_device_gets_no_retry() {
  let Some(scene) = Scene::start(|settings| {
    settings.push.retry.initial_delay = Duration::from_millis(300);
  })
  .await
  else {
    return;
  };
  scene.fake.set_reply(Some(outage()));
  send(&scene.app, &scene.alice, &scene.channel, &scene.alice_device, 1).await;
  scene.fake.wait_for_attempts(1).await;
  let reply = scene
    .app
    .delete(&scene.carol, &format!("/devices/{}", scene.carol_device))
    .await;
  assert!(reply.status.is_success(), "{}", reply.body);
  scene.fake.set_reply(None);
  tokio::time::sleep(Duration::from_millis(1000)).await;
  assert!(scene.fake.pushes().is_empty());
  assert_eq!(scene.queued().await, 0);
  scene.app.stop().await;
}

#[tokio::test]
async fn a_message_read_meanwhile_is_not_retried() {
  let Some(scene) = Scene::start(|settings| {
    settings.push.retry.initial_delay = Duration::from_millis(600);
  })
  .await
  else {
    return;
  };
  scene.fake.set_reply(Some(outage()));
  let message = send(&scene.app, &scene.alice, &scene.channel, &scene.alice_device, 1).await;
  scene.fake.wait_for_attempts(1).await;
  wait_until(|| async { scene.queued().await == 1 }).await;
  assert_eq!(scene.queued().await, 1);

  // carol reads the channel on her desktop before the push service is back.
  let reply = scene
    .app
    .put(
      &scene.carol,
      &format!("/conversations/{}/read-position", scene.channel),
      json!({"message_id": message}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
  scene.fake.set_reply(None);
  wait_until(|| async { scene.queued().await == 0 }).await;
  tokio::time::sleep(Duration::from_millis(300)).await;
  assert_eq!(scene.queued().await, 0);
  assert!(
    scene.fake.pushes().is_empty(),
    "nothing shows a message already read"
  );
  scene.app.stop().await;
}

/// alice sends to a channel; carol has an FCM device behind the fake gateway.
struct Scene {
  app: TestApp,
  fake: FakeGateway,
  url: String,
  channel: String,
  alice: TestUser,
  carol: TestUser,
  alice_device: String,
  carol_device: String,
}

impl Scene {
  async fn start(customize: impl FnOnce(&mut Settings)) -> Option<Self> {
    let (fake, url) = FakeGateway::start().await;
    let app = TestApp::start_with_push(
      |settings| {
        settings.push.retry.initial_delay = Duration::from_millis(50);
        settings.push.retry.max_delay = Duration::from_millis(200);
        settings.push.retry.poll_interval = Duration::from_millis(20);
        customize(settings);
      },
      dispatcher(&url),
    )
    .await?;
    let (alice, carol) = (owner(), member("carol"));
    let channel = channel_with(&app, &[&carol]).await;
    let alice_device = register_device(&app, &alice).await;
    let carol_device = register_device(&app, &carol).await;
    let carol_id = user_id(&app, &carol).await;
    let reply = app
      .patch(
        &carol,
        &format!("/conversations/{channel}/members/{carol_id}"),
        json!({"notification_level": "all"}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    let server_id = app.state.push.gateway_binding().unwrap().server_id.clone();
    let reply = app
      .put(
        &carol,
        &format!("/devices/{carol_device}/push"),
        json!({
          "provider": "fcm",
          "token": sealed_token(&fake, &server_id),
          "push_key": STANDARD.encode(PUSH_KEY),
        }),
      )
      .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
    Some(Self {
      app,
      fake,
      url,
      channel,
      alice,
      carol,
      alice_device,
      carol_device,
    })
  }

  /// The dispatcher of a (re)started server: same gateway, same key.
  fn dispatcher(&self) -> PushDispatcher {
    dispatcher(&self.url)
  }

  async fn queued(&self) -> u64 {
    self
      .app
      .state
      .database
      .count_push_retries(self.carol_device.parse::<DeviceId>().unwrap())
      .await
      .unwrap()
  }
}

fn dispatcher(url: &str) -> PushDispatcher {
  let client = GatewayClient::new(GatewaySettings {
    url: url.to_owned(),
    key: parse_gateway_key(&URL_SAFE_NO_PAD.encode(SERVER_KEY)).unwrap(),
    timeout: Duration::from_secs(5),
    accept_unbound_tokens: false,
  })
  .unwrap();
  PushDispatcher::new(4)
    .with_sender(&PushProvider::Fcm, Arc::new(client.sender(PushProvider::Fcm)))
    .with_gateway_binding(teamcraft_server::push::GatewayBinding {
      url: client.url().to_owned(),
      server_id: client.server_id().to_owned(),
      providers: vec![PushProvider::Fcm],
    })
}

fn sealed_token(fake: &FakeGateway, server_id: &str) -> String {
  let expires_at_ms = u64::try_from(
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap()
      .as_millis(),
  )
  .unwrap()
    + 3_600_000;
  let blob = seal_bound_token(
    &fake.token_public_key(),
    "fcm",
    server_id,
    expires_at_ms,
    FCM_TOKEN,
  )
  .unwrap();
  format!("{BOUND_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(blob))
}

/// Apple or Google down, as the gateway reports it.
fn outage() -> (u16, serde_json::Value) {
  (
    502,
    json!({"error_code": "provider_unavailable", "message": "down"}),
  )
}

async fn wait_until<F, Fut>(condition: F)
where
  F: Fn() -> Fut,
  Fut: std::future::Future<Output = bool>,
{
  for _ in 0..200 {
    if condition().await {
      return;
    }
    tokio::time::sleep(Duration::from_millis(25)).await;
  }
}

/// Queued rings stop once the call stops ringing (issue #142).
mod rings {
  use std::path::PathBuf;

  use teamcraft_db::NewPushRetry;
  use teamcraft_proto::{CallId, ConversationId, PushWakeUp, UserId};
  use teamcraft_server::config::CallsConfig;

  use super::*;

  /// Throwaway Ed25519 key used only by these tests. It protects nothing.
  const SIGNING_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MC4CAQAwBQYDK2VwBCIEIAxxS2yuWa9SUUAfZhI0772UQ48VXZLz45+L2A5IfcWA
-----END PRIVATE KEY-----
";
  const SFU_TOKEN: &str = "sfu-registration-token-for-tests";

  #[tokio::test]
  async fn a_ring_queued_during_an_outage_is_dropped_when_the_call_is_answered() {
    let Some(scene) = RingScene::start().await else {
      return;
    };
    scene.fake.set_reply(Some(outage()));
    let call = scene.ring().await;
    scene.fake.wait_for_attempts(1).await;
    wait_until(|| async { scene.queued_rings().await == 1 }).await;
    assert_eq!(scene.queued_rings().await, 1, "the ring waits for its retry");

    // carol answers on her desktop: the ring ends and its queued retry goes with it.
    scene
      .report("joined", &call, &scene.carol_id, &scene.carol_desktop)
      .await;
    wait_until(|| async { scene.queued_rings().await == 0 }).await;
    assert_eq!(scene.queued_rings().await, 0);
    // What is left is the dismissal for the phone that was shown the ring; it waits for the
    // provider like any wake-up.
    assert!(scene.fake.pushes().is_empty(), "the phone never rings late");
    scene.app.stop().await;
  }

  #[tokio::test]
  async fn the_worker_drops_a_ring_whose_call_no_longer_rings() {
    let Some(scene) = RingScene::start().await else {
      return;
    };
    // A ring left in the queue (written before the ring ended, or before a restart) for a call
    // that doesn't ring: the worker claims it and drops it without sending.
    let state = &scene.app.state;
    let device = state
      .database
      .list_push_devices(scene.carol_id.parse::<UserId>().unwrap())
      .await
      .unwrap()
      .into_iter()
      .next()
      .unwrap();
    let retry = NewPushRetry {
      id: state.next_id().unwrap(),
      user: scene.carol_id.parse().unwrap(),
      device: device.device,
      is_user_endpoint: false,
      wake_up: PushWakeUp {
        kind: PushKind::Call,
        conversation_id: scene.dm.parse::<ConversationId>().unwrap(),
        maybe_message_id: None,
        maybe_sender_id: None,
        is_urgent: false,
        maybe_thread_id: None,
        maybe_call_id: Some("77".parse::<CallId>().unwrap()),
        maybe_badge: None,
        maybe_sent_at: None,
      },
      attempts: 1,
      delay: Duration::ZERO,
      ttl: Duration::from_secs(30),
    };
    assert!(state.database.enqueue_push_retry(&retry, 10).await.unwrap());
    wait_until(|| async { scene.queued().await == 0 }).await;
    assert_eq!(scene.queued().await, 0);
    assert_eq!(scene.fake.push_attempts(), 0, "nothing was sent");
    scene.app.stop().await;
  }

  /// alice calls carol in a DM; carol has a phone behind the fake gateway and a desktop.
  struct RingScene {
    app: TestApp,
    fake: FakeGateway,
    dm: String,
    alice_id: String,
    alice_device: String,
    carol_id: String,
    carol_device: String,
    carol_desktop: String,
  }

  impl RingScene {
    async fn start() -> Option<Self> {
      let (fake, url) = FakeGateway::start().await;
      let app =
        TestApp::start_with_calls_push_and(&calls_config(), SIGNING_KEY_PEM, dispatcher(&url), |settings| {
          // Long enough that the ring is still queued when the test ends the call.
          settings.push.retry.initial_delay = Duration::from_secs(20);
          settings.push.retry.max_delay = Duration::from_secs(20);
          settings.push.retry.poll_interval = Duration::from_millis(20);
        })
        .await?;
      let (alice, carol) = (owner(), member("carol"));
      let carol_id = user_id(&app, &carol).await;
      let alice_id = user_id(&app, &alice).await;
      let dm = app.post(&alice, "/dms", json!({"user_id": carol_id})).await;
      assert!(dm.status.is_success(), "{}", dm.body);
      let dm = dm.id();
      let alice_device = register_device(&app, &alice).await;
      let carol_device = register_device(&app, &carol).await;
      let carol_desktop = register_device(&app, &carol).await;
      let server_id = app.state.push.gateway_binding().unwrap().server_id.clone();
      let reply = app
        .put(
          &carol,
          &format!("/devices/{carol_device}/push"),
          json!({
            "provider": "fcm",
            "token": sealed_token(&fake, &server_id),
            "push_key": STANDARD.encode(PUSH_KEY),
          }),
        )
        .await;
      assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.body);
      Some(Self {
        app,
        fake,
        dm,
        alice_id,
        alice_device,
        carol_id,
        carol_device,
        carol_desktop,
      })
    }

    /// alice starts the call, which rings carol. Returns the call ID.
    async fn ring(&self) -> String {
      let reply = self
        .app
        .post(
          &owner(),
          &format!("/conversations/{}/call/join-token", self.dm),
          json!({ "device_id": self.alice_device }),
        )
        .await;
      assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
      let call = reply.body["call_id"].as_str().unwrap().to_owned();
      self
        .report("joined", &call, &self.alice_id, &self.alice_device)
        .await;
      call
    }

    async fn report(&self, action: &str, call_id: &str, user_id: &str, device_id: &str) {
      let status = self
        .app
        .http
        .post(format!(
          "{}/api/internal/v1/calls/participants/{action}",
          self.app.base_url
        ))
        .bearer_auth(SFU_TOKEN)
        .json(&json!({ "call_id": call_id, "user_id": user_id, "device_id": device_id }))
        .send()
        .await
        .unwrap()
        .status();
      assert_eq!(status, StatusCode::NO_CONTENT);
    }

    async fn queued(&self) -> u64 {
      self
        .app
        .state
        .database
        .count_push_retries(self.carol_device.parse::<DeviceId>().unwrap())
        .await
        .unwrap()
    }
    /// Queued rings only (a dismissal may wait in the queue too).
    async fn queued_rings(&self) -> i64 {
      teamcraft_db::testing::sqlx::query_scalar("SELECT COUNT(*) FROM push_retries WHERE kind = 'call'")
        .fetch_one(self.app.sql_pool())
        .await
        .unwrap()
    }
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
    }
  }
}
