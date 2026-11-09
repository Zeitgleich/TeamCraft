//! Outgoing webhooks (docs/webhooks.md) against a local fake receiver: signed metadata events,
//! backoff of a failing target as a whole, capped attempts, the bounded queue, a delivery that
//! survives a crash mid-send, and the SSRF refusals of the console. The tests drive the delivery
//! queue themselves (`deliver_next`), so nothing depends on timing.
#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use crate::common::*;

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode as AxumStatus};
use axum::response::IntoResponse;
use axum::routing::post;
use reqwest::StatusCode;
use serde_json::{Value, json};
use teamcraft_crypto::webhook::{WebhookMessage, WebhookSecret};
use teamcraft_proto::{WebhookEventData, WebhookEventKind};
use teamcraft_server::state::Settings;
use teamcraft_server::testing::TestApp;
use teamcraft_server::webhooks::{WebhookDispatcher, deliver_next, emit};

const TOLERANCE_SECS: i64 = 300;
const ALL_EVENTS: [&str; 8] = [
  "channel.created",
  "channel.archived",
  "channel.unarchived",
  "channel.deleted",
  "member.joined",
  "member.left",
  "call.started",
  "call.ended",
];

#[tokio::test]
async fn a_channel_event_arrives_signed_and_without_names() {
  let Some(app) = start(|_| {}).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, secret) = create(&app, &receiver.url, &["channel.created", "member.joined"]).await;
  let space = create_space(&app, &owner(), "Project A space", "open").await;
  let channel = create_channel(&app, &owner(), &space, "project-a-secret-name", "public").await;
  // A direct conversation is private: nothing about it goes out.
  let bob_id = user_id(&app, &member("bob")).await;
  let dm = app.post(&owner(), "/dms", json!({"user_id": bob_id})).await;
  assert!(dm.status.is_success(), "{}", dm.body);

  assert!(deliver_next(&app.state).await.unwrap());
  assert!(
    !deliver_next(&app.state).await.unwrap(),
    "only the channel event was queued"
  );
  let requests = receiver.requests();
  assert_eq!(requests.len(), 1);
  let request = &requests[0];
  let body: Value = serde_json::from_slice(&request.body).unwrap();
  assert_eq!(body["type"], "channel.created");
  assert_eq!(body["data"]["conversation_id"], channel.as_str());
  assert_eq!(body["id"], request.header("webhook-id"));
  let text = String::from_utf8(request.body.clone()).unwrap();
  assert!(
    !text.contains("project-a-secret-name"),
    "names never leave the server"
  );
  assert!(!text.contains("Project A"));
  assert_eq!(request.header("content-type"), "application/json");

  let secret = WebhookSecret::from_text(&secret).unwrap();
  let timestamp: i64 = request.header("webhook-timestamp").parse().unwrap();
  let message = WebhookMessage {
    id: request.header("webhook-id"),
    timestamp_secs: timestamp,
    body: &request.body,
  };
  secret
    .verify(
      &message,
      request.header("webhook-signature"),
      now_secs() as i64,
      TOLERANCE_SECS,
    )
    .unwrap();

  let listed = app.get(&admin(), "/admin/webhooks").await;
  let health = &listed.body["webhooks"][0]["health"];
  assert!(health["last_success_at"].is_i64());
  assert_eq!(health["consecutive_failures"], 0);
  let log = app
    .get(&admin(), &format!("/admin/webhooks/{webhook}/deliveries"))
    .await;
  assert_eq!(log.body["entries"][0]["outcome"], "delivered");
  assert_eq!(log.body["entries"][0]["status_code"], 204);
  app.stop().await;
}

#[tokio::test]
async fn a_failing_target_backs_off_as_a_whole_and_gets_the_events_in_order_afterwards() {
  let Some(app) = start(|_| {}).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, _) = create(&app, &receiver.url, &ALL_EVENTS).await;
  receiver.reply_with(503);
  for _ in 0..3 {
    emit(
      &app.state,
      WebhookEventKind::ChannelArchived,
      WebhookEventData::default(),
    )
    .await;
  }

  assert!(deliver_next(&app.state).await.unwrap());
  // The webhook is paused now: no attempt per queued event while the target is down.
  assert!(!deliver_next(&app.state).await.unwrap());
  assert_eq!(receiver.requests().len(), 1);
  let health = health_of(&app, &webhook).await;
  assert_eq!(health["consecutive_failures"], 1);
  assert_eq!(health["queued"], 3);
  assert!(health["paused_until"].is_i64());
  assert_eq!(health["last_error"], "HTTP 503");

  // The backoff is over and the target is back: the events arrive oldest first, the failed one
  // again with the same ID.
  end_backoff(&app).await;
  receiver.reply_with(204);
  for _ in 0..3 {
    assert!(deliver_next(&app.state).await.unwrap());
  }
  let ids: Vec<u64> = receiver
    .requests()
    .iter()
    .map(|request| request.header("webhook-id").parse().unwrap())
    .collect();
  assert_eq!(ids[0], ids[1], "the retry carries the same idempotency key");
  assert!(ids[1] < ids[2] && ids[2] < ids[3], "in event order: {ids:?}");
  let health = health_of(&app, &webhook).await;
  assert_eq!(health["consecutive_failures"], 0);
  assert_eq!(health["queued"], 0);
  assert!(health.get("paused_until").is_none());
  let log = app
    .get(&admin(), &format!("/admin/webhooks/{webhook}/deliveries"))
    .await;
  let outcomes: Vec<&str> = log.body["entries"]
    .as_array()
    .unwrap()
    .iter()
    .map(|entry| entry["outcome"].as_str().unwrap())
    .collect();
  assert_eq!(outcomes, ["delivered", "delivered", "delivered", "retrying"]);
  app.stop().await;
}

#[tokio::test]
async fn attempts_are_capped_and_a_refusal_drops_the_event_at_once() {
  let Some(app) = start(|settings| settings.webhooks.max_attempts = 2).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, _) = create(&app, &receiver.url, &ALL_EVENTS).await;
  receiver.reply_with(500);
  emit(
    &app.state,
    WebhookEventKind::MemberJoined,
    WebhookEventData::default(),
  )
  .await;
  assert!(deliver_next(&app.state).await.unwrap());
  end_backoff(&app).await;
  assert!(deliver_next(&app.state).await.unwrap());
  let health = health_of(&app, &webhook).await;
  assert_eq!(health["queued"], 0, "given up after two attempts");
  assert_eq!(health["dropped"], 1);

  // 4xx: the target refuses; trying again won't help, and the webhook is not paused for it.
  end_backoff(&app).await;
  receiver.reply_with(410);
  emit(
    &app.state,
    WebhookEventKind::MemberLeft,
    WebhookEventData::default(),
  )
  .await;
  assert!(deliver_next(&app.state).await.unwrap());
  let health = health_of(&app, &webhook).await;
  assert_eq!(health["queued"], 0);
  assert_eq!(health["dropped"], 2);
  assert!(health.get("paused_until").is_none());
  assert_eq!(receiver.requests().len(), 3);
  app.stop().await;
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
  let Some(app) = start(|_| {}).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, _) = create(&app, &receiver.url, &ALL_EVENTS).await;
  receiver.reply_with(307);
  emit(
    &app.state,
    WebhookEventKind::CallEnded,
    WebhookEventData::default(),
  )
  .await;
  assert!(deliver_next(&app.state).await.unwrap());
  assert_eq!(receiver.requests().len(), 1);
  assert_eq!(health_of(&app, &webhook).await["dropped"], 1);
  app.stop().await;
}

#[tokio::test]
async fn the_queue_of_a_webhook_is_bounded() {
  let Some(app) = start(|settings| settings.webhooks.max_queued_per_webhook = 3).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, _) = create(&app, &receiver.url, &ALL_EVENTS).await;
  receiver.reply_with(503);
  for _ in 0..5 {
    emit(
      &app.state,
      WebhookEventKind::ChannelCreated,
      WebhookEventData::default(),
    )
    .await;
  }
  let health = health_of(&app, &webhook).await;
  assert_eq!(health["queued"], 3);
  assert_eq!(health["dropped"], 2);
  app.stop().await;
}

#[tokio::test]
async fn a_delivery_cut_off_by_a_crash_is_sent_again_after_the_restart() {
  let Some(mut app) = start(|_| {}).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, _) = create(&app, &receiver.url, &ALL_EVENTS).await;
  emit(
    &app.state,
    WebhookEventKind::CallStarted,
    WebhookEventData::default(),
  )
  .await;

  // The attempt hangs at the target and the process dies meanwhile: the claim's transaction goes
  // with it, the event stays queued.
  receiver.hang(true);
  let state = app.state.clone();
  let in_flight = tokio::spawn(async move { deliver_next(&state).await });
  receiver.wait_for(1).await;
  in_flight.abort();
  let _ = in_flight.await;

  // A new process: fresh memory, same database.
  app.state.webhooks = Arc::new(WebhookDispatcher::new(&app.state.settings.get().webhooks).unwrap());
  receiver.hang(false);
  let mut delivered = false;
  for _ in 0..50 {
    // The aborted claim's connection is rolled back asynchronously; until then the row is locked.
    if deliver_next(&app.state).await.unwrap() {
      delivered = true;
      break;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
  assert!(delivered);
  let requests = receiver.requests();
  assert_eq!(requests.len(), 2);
  assert_eq!(requests[0].header("webhook-id"), requests[1].header("webhook-id"));
  assert_eq!(health_of(&app, &webhook).await["queued"], 0);
  app.stop().await;
}

#[tokio::test]
async fn a_hanging_target_does_not_hold_up_new_events() {
  let Some(app) = start(|_| {}).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, _) = create(&app, &receiver.url, &ALL_EVENTS).await;
  emit(
    &app.state,
    WebhookEventKind::MemberJoined,
    WebhookEventData::default(),
  )
  .await;
  receiver.hang(true);
  let state = app.state.clone();
  let in_flight = tokio::spawn(async move { deliver_next(&state).await });
  receiver.wait_for(1).await;
  // The delivery holds its claim; a new event (a join, a new channel) must not wait for it.
  tokio::time::timeout(
    Duration::from_secs(2),
    emit(
      &app.state,
      WebhookEventKind::MemberLeft,
      WebhookEventData::default(),
    ),
  )
  .await
  .expect("the event was held up by the delivery in flight");
  assert_eq!(
    app
      .state
      .database
      .count_webhook_deliveries(webhook.parse().unwrap())
      .await
      .unwrap(),
    2
  );
  receiver.hang(false);
  assert!(in_flight.await.unwrap().unwrap());
  app.stop().await;
}

#[tokio::test]
async fn a_new_url_starts_with_an_empty_queue() {
  let Some(app) = start(|_| {}).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, _) = create(&app, &receiver.url, &ALL_EVENTS).await;
  emit(
    &app.state,
    WebhookEventKind::ChannelCreated,
    WebhookEventData::default(),
  )
  .await;
  let other = Receiver::start().await;
  let reply = app
    .put(
      &admin(),
      &format!("/admin/webhooks/{webhook}"),
      json!({"url": other.url, "events": ["channel.created"], "is_enabled": true}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  assert_eq!(reply.body["health"]["queued"], 0);
  assert!(!deliver_next(&app.state).await.unwrap());
  app.stop().await;
}

#[tokio::test]
async fn disabling_a_webhook_empties_its_queue_and_stops_new_events() {
  let Some(app) = start(|_| {}).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, _) = create(&app, &receiver.url, &ALL_EVENTS).await;
  emit(
    &app.state,
    WebhookEventKind::ChannelCreated,
    WebhookEventData::default(),
  )
  .await;
  let reply = app
    .put(
      &admin(),
      &format!("/admin/webhooks/{webhook}"),
      json!({"url": receiver.url, "events": ["channel.created"], "is_enabled": false}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  assert_eq!(reply.body["health"]["queued"], 0);
  emit(
    &app.state,
    WebhookEventKind::ChannelCreated,
    WebhookEventData::default(),
  )
  .await;
  assert!(!deliver_next(&app.state).await.unwrap());
  assert!(receiver.requests().is_empty());
  app.stop().await;
}

#[tokio::test]
async fn the_console_test_sends_a_signed_ping() {
  let Some(app) = start(|_| {}).await else {
    return;
  };
  let receiver = Receiver::start().await;
  let (webhook, secret) = create(&app, &receiver.url, &["member.joined"]).await;
  let reply = app
    .post(&admin(), &format!("/admin/webhooks/{webhook}/test"), json!({}))
    .await;
  assert_eq!(reply.status, StatusCode::OK);
  assert_eq!(reply.body["is_ok"], true);
  let request = &receiver.requests()[0];
  assert_eq!(
    serde_json::from_slice::<Value>(&request.body).unwrap()["type"],
    "ping"
  );

  // A rotated secret replaces the old one at once.
  let rotated = app
    .post(&admin(), &format!("/admin/webhooks/{webhook}/secret"), json!({}))
    .await;
  let new_secret = rotated.body["secret"].as_str().unwrap();
  assert_ne!(new_secret, secret);
  app
    .post(&admin(), &format!("/admin/webhooks/{webhook}/test"), json!({}))
    .await;
  let request = &receiver.requests()[1];
  let message = WebhookMessage {
    id: request.header("webhook-id"),
    timestamp_secs: request.header("webhook-timestamp").parse().unwrap(),
    body: &request.body,
  };
  let signature = request.header("webhook-signature");
  let now = now_secs() as i64;
  assert!(
    WebhookSecret::from_text(&secret)
      .unwrap()
      .verify(&message, signature, now, TOLERANCE_SECS)
      .is_err()
  );
  WebhookSecret::from_text(new_secret)
    .unwrap()
    .verify(&message, signature, now, TOLERANCE_SECS)
    .unwrap();
  app.stop().await;
}

mod console {
  use super::*;

  #[tokio::test]
  async fn private_and_plain_targets_are_refused_unless_the_operator_allows_them() {
    let Some(app) = TestApp::start().await else {
      return;
    };
    for url in [
      "https://127.0.0.1/hook",
      "https://10.0.0.8/hook",
      "https://192.168.1.10/hook",
      "https://169.254.169.254/latest/meta-data",
      "https://[::1]/hook",
      "https://[fe80::1]/hook",
      "https://[fd00::1]/hook",
      "https://[::ffff:127.0.0.1]/hook",
      "https://localhost/hook",
      "https://intranet/hook",
      "http://hooks.example.com/hook",
      "https://user:pw@hooks.example.com/hook",
    ] {
      let reply = app
        .post(
          &admin(),
          "/admin/webhooks",
          json!({"url": url, "events": ["member.joined"], "is_enabled": true}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{url}: {}", reply.body);
    }
    let listed = app.get(&admin(), "/admin/webhooks").await;
    assert_eq!(listed.body["allows_private_targets"], false);
    assert_eq!(listed.body["webhooks"], json!([]));
    // A public target is fine.
    let reply = app
      .post(
        &admin(),
        "/admin/webhooks",
        json!({"url": "https://hooks.example.com/teamcraft", "events": ["member.joined"], "is_enabled": false}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    app.stop().await;
  }

  #[tokio::test]
  async fn bad_events_and_non_admins_are_refused() {
    let Some(app) = start(|_| {}).await else {
      return;
    };
    for events in [json!([]), json!(["ping"]), json!(["message.sent"])] {
      let reply = app
        .post(
          &admin(),
          "/admin/webhooks",
          json!({"url": "https://hooks.example.com/x", "events": events, "is_enabled": true}),
        )
        .await;
      assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{events}");
    }
    let reply = app.get(&member("mallory"), "/admin/webhooks").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let reply = app
      .post(
        &member("mallory"),
        "/admin/webhooks",
        json!({"url": "https://hooks.example.com/x", "events": ["member.joined"], "is_enabled": true}),
      )
      .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let reply = app.delete(&admin(), "/admin/webhooks/123").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.body["error_code"], "webhook_not_found");
    app.stop().await;
  }

  #[tokio::test]
  async fn the_secret_is_shown_once_and_never_listed() {
    let Some(app) = start(|_| {}).await else {
      return;
    };
    let (webhook, secret) = create(&app, "https://hooks.example.com/x", &["member.joined"]).await;
    assert!(secret.starts_with("whsec_"));
    let listed = app.get(&admin(), "/admin/webhooks").await;
    assert!(!listed.body.to_string().contains(&secret[6..]));
    let audit = app.get(&admin(), "/admin/audit").await;
    assert!(!audit.body.to_string().contains(&secret[6..]));
    let reply = app.delete(&admin(), &format!("/admin/webhooks/{webhook}")).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    app.stop().await;
  }
}

/// A server whose webhooks may reach the fake receiver on this machine.
async fn start(customize: impl FnOnce(&mut Settings)) -> Option<TestApp> {
  TestApp::start_with(|settings| {
    settings.webhooks.allow_private_targets = true;
    customize(settings);
  })
  .await
}

/// Creates a webhook; returns its ID and secret.
async fn create(app: &TestApp, url: &str, events: &[&str]) -> (String, String) {
  let reply = app
    .post(
      &admin(),
      "/admin/webhooks",
      json!({"url": url, "description": "CI", "events": events, "is_enabled": true}),
    )
    .await;
  assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
  (
    reply.body["webhook"]["id"].as_str().unwrap().to_owned(),
    reply.body["secret"].as_str().unwrap().to_owned(),
  )
}

async fn health_of(app: &TestApp, webhook: &str) -> Value {
  let listed = app.get(&admin(), "/admin/webhooks").await;
  listed.body["webhooks"]
    .as_array()
    .unwrap()
    .iter()
    .find(|entry| entry["id"] == webhook)
    .unwrap()["health"]
    .clone()
}

/// Ends every backoff, as if its time had passed.
async fn end_backoff(app: &TestApp) {
  teamcraft_db::testing::sqlx::query("UPDATE webhooks SET paused_until = NULL")
    .execute(app.sql_pool())
    .await
    .unwrap();
}

#[derive(Debug, Clone)]
struct ReceivedRequest {
  headers: HeaderMap,
  body: Vec<u8>,
}

impl ReceivedRequest {
  fn header(&self, name: &str) -> &str {
    self
      .headers
      .get(name)
      .and_then(|value| value.to_str().ok())
      .unwrap_or("")
  }
}

/// A webhook receiver on this machine that records requests and answers with a set status, or
/// hangs.
#[derive(Clone)]
struct Receiver {
  url: String,
  received: Arc<Mutex<Vec<ReceivedRequest>>>,
  status: Arc<AtomicU16>,
  is_hanging: Arc<AtomicBool>,
}

impl Receiver {
  async fn start() -> Self {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let receiver = Self {
      url: format!("http://{address}/hooks/teamcraft"),
      received: Arc::default(),
      status: Arc::new(AtomicU16::new(204)),
      is_hanging: Arc::default(),
    };
    let router = Router::new()
      .route(
        "/hooks/teamcraft",
        post(
          |State(receiver): State<Receiver>, headers: HeaderMap, body: axum::body::Bytes| async move {
            receiver.received.lock().unwrap().push(ReceivedRequest {
              headers,
              body: body.to_vec(),
            });
            while receiver.is_hanging.load(Ordering::SeqCst) {
              tokio::time::sleep(Duration::from_millis(20)).await;
            }
            let status = AxumStatus::from_u16(receiver.status.load(Ordering::SeqCst)).unwrap();
            // A redirect points at the cloud metadata address: it must not be followed.
            (status, [("location", "http://169.254.169.254/")]).into_response()
          },
        ),
      )
      .with_state(receiver.clone());
    tokio::spawn(async move {
      axum::serve(listener, router).await.unwrap();
    });
    receiver
  }

  fn reply_with(&self, status: u16) {
    self.status.store(status, Ordering::SeqCst);
  }

  fn hang(&self, is_hanging: bool) {
    self.is_hanging.store(is_hanging, Ordering::SeqCst);
  }

  fn requests(&self) -> Vec<ReceivedRequest> {
    self.received.lock().unwrap().clone()
  }

  async fn wait_for(&self, count: usize) {
    for _ in 0..200 {
      if self.requests().len() >= count {
        return;
      }
      tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the receiver got fewer than {count} requests");
  }
}
