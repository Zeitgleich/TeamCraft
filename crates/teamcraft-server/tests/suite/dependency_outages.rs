//! Fault injection against fakes (issue #743): object storage, push gateway and SFU control fail on
//! demand, and the tests check that the server degrades gracefully — retries, then a breaker that
//! fails fast with 503 and `Retry-After`, readiness that reports it — and recovers by itself once
//! the dependency is back. Time is a manual clock the tests step; nothing sleeps.
//!
//! The storage cases at the store level and the database gate need nothing else; the HTTP, push and
//! SFU cases need the dev Postgres (`--features integration`). The heavier cases against the real binaries (Postgres
//! cut, identity provider cut, server restarts) are in the nightly e2e suite
//! (`tests/e2e/tests/suite/dependency_outages.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use secrecy::SecretString;
use teamcraft_proto::{DependencyName, DependencyState};
use teamcraft_server::config::StorageConfig;
use teamcraft_server::dependencies::testing::ManualClock;
use teamcraft_server::dependencies::{Dependencies, DependencyGuard, DependencyOptions};
use teamcraft_server::storage::{BlobStore, StorageError};

/// The breaker's open time with the default options.
const OPEN_FOR: Duration = Duration::from_secs(15);

mod storage {
  use super::*;

  /// The storage answers 503 (a provider outage): each call is retried, after three failed calls
  /// the breaker opens and no request reaches the storage any more until the next probe; once the
  /// storage is back, the probe closes it.
  #[tokio::test]
  async fn an_outage_opens_the_breaker_and_a_probe_closes_it_again() {
    let (fake, store, guard, clock) = fake_store().await;
    fake.set_failing(true);
    for _ in 0..3 {
      assert!(matches!(
        store.object_size("avatars/1").await,
        Err(StorageError::Status(StatusCode::SERVICE_UNAVAILABLE))
      ));
    }
    assert_eq!(fake.requests(), 9, "three calls, two retries each");
    assert_eq!(guard.state(), DependencyState::Down);

    let fast = store.object_size("avatars/1").await;
    assert!(
      matches!(&fast, Err(StorageError::Unavailable(open)) if open.retry_after == OPEN_FOR),
      "{fast:?}"
    );
    assert!(store.check_available().is_err(), "no upload URL while it is down");
    assert_eq!(
      fake.requests(),
      9,
      "the open breaker answered without asking the storage"
    );

    fake.set_failing(false);
    clock.advance(OPEN_FOR);
    assert_eq!(guard.state(), DependencyState::Recovering);
    assert!(store.check_available().is_ok(), "the next upload may probe it");
    assert_eq!(store.object_size("avatars/1").await.unwrap(), None);
    assert_eq!(guard.state(), DependencyState::Ok);
  }

  /// One 503 in between is retried within the call and never reaches the caller.
  #[tokio::test]
  async fn a_single_hiccup_is_retried_away() {
    let (fake, store, guard, _clock) = fake_store().await;
    fake.fail_next(1);
    store
      .put_object("brand/logo", vec![1, 2, 3], "image/png")
      .await
      .unwrap();
    assert_eq!(fake.requests(), 2);
    assert_eq!(fake.stored("brand/logo"), Some(3));
    assert_eq!(guard.state(), DependencyState::Ok);
  }

  /// A refusal (4xx) is an answer: it is not retried and doesn't count against the storage.
  #[tokio::test]
  async fn refusals_are_not_outages() {
    let (fake, store, guard, _clock) = fake_store().await;
    fake.set_refusing(true);
    for _ in 0..5 {
      assert!(store.get_object("brand/logo", 10).await.is_err());
    }
    assert_eq!(fake.requests(), 5);
    assert_eq!(guard.state(), DependencyState::Ok);
  }

  /// Starting an upload in parts creates something each time: never repeated.
  #[tokio::test]
  async fn creating_an_upload_in_parts_is_not_repeated() {
    let (fake, store, _guard, _clock) = fake_store().await;
    fake.set_failing(true);
    assert!(store.create_multipart("attachments/1").await.is_err());
    assert_eq!(fake.requests(), 1);
  }
}

#[cfg(feature = "integration")]
mod http {
  use futures_util::{SinkExt, StreamExt};
  use serde_json::json;
  use teamcraft_proto::{
    DeviceActivity, ErrorBody, ErrorCode, GatewayClientMessage, GatewayServerMessage, HealthResponse,
    HealthStatus,
  };
  use teamcraft_server::testing::TestApp;
  use tokio_tungstenite::tungstenite::Message;

  use super::*;
  use crate::common::member;

  /// The API side of a storage outage: completing an avatar fails with 503 and `Retry-After`, after
  /// three failures new uploads are refused at once (no URL that would fail), `/readyz` stays 200
  /// but says `degraded` with the storage down, and after the storage is back an upload goes
  /// through and readiness is `ok` again, without a restart.
  #[tokio::test]
  async fn a_storage_outage_answers_503_with_retry_after_and_heals() {
    let (fake, endpoint) = FakeStorage::start().await;
    let clock = ManualClock::new();
    let dependencies = Arc::new(Dependencies::new(
      DependencyOptions::default(),
      Arc::new(clock.clone()),
    ));
    let Some(app) =
      TestApp::start_with_dependencies(Some(storage_config(&endpoint)), None, dependencies).await
    else {
      return;
    };
    let carol = member("carol");
    let upload = start_avatar_upload(&app, &carol).await;
    assert_eq!(upload.status, StatusCode::CREATED, "{}", upload.body);
    let url = upload.body["upload_url"].as_str().unwrap().to_owned();

    fake.set_failing(true);
    for _ in 0..3 {
      let reply = app.post(&carol, "/me/avatar/complete", json!({})).await;
      assert_unavailable(&reply.status, &reply.body);
    }
    let refused = start_avatar_upload(&app, &carol).await;
    assert_unavailable(&refused.status, &refused.body);
    let ready = readiness(&app).await;
    assert_eq!(ready.status, HealthStatus::Degraded);
    assert!(
      ready
        .dependencies
        .iter()
        .any(|health| health.dependency == DependencyName::ObjectStorage
          && health.state == DependencyState::Down),
      "{ready:?}"
    );

    fake.set_failing(false);
    clock.advance(OPEN_FOR);
    let upload = start_avatar_upload(&app, &carol).await;
    assert_eq!(upload.status, StatusCode::CREATED, "{}", upload.body);
    let url_after = upload.body["upload_url"].as_str().unwrap().to_owned();
    assert_ne!(url, url_after);
    put(&url_after, 4).await;
    let completed = app.post(&carol, "/me/avatar/complete", json!({})).await;
    assert_eq!(completed.status, StatusCode::OK, "{}", completed.body);
    assert_eq!(readiness(&app).await.status, HealthStatus::Ok);
    app.stop().await;
  }

  /// The sweeper deletes rows before objects: while the storage is down it leaves both for the
  /// next sweep instead of losing track of objects it can't delete.
  #[tokio::test]
  async fn nothing_is_swept_while_the_storage_is_down() {
    let (fake, endpoint) = FakeStorage::start().await;
    let clock = ManualClock::new();
    let dependencies = Arc::new(Dependencies::new(
      DependencyOptions::default(),
      Arc::new(clock.clone()),
    ));
    let Some(app) =
      TestApp::start_with_dependencies(Some(storage_config(&endpoint)), None, dependencies).await
    else {
      return;
    };
    let carol = member("carol");
    let abandoned = app
      .post(&carol, "/me/backgrounds", json!({"size_bytes": 10}))
      .await;
    assert_eq!(abandoned.status, StatusCode::CREATED, "{}", abandoned.body);
    let store = app.state.maybe_blob_store.clone().unwrap();
    fake.set_failing(true);
    for _ in 0..3 {
      assert!(store.object_size("probe").await.is_err());
    }
    let later = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    let sweep = || teamcraft_server::services::backgrounds::sweep(&app.state, later);
    assert_eq!(sweep().await.unwrap(), 0, "swept while the storage is down");

    fake.set_failing(false);
    clock.advance(OPEN_FOR);
    assert_eq!(sweep().await.unwrap(), 1, "the next sweep after the outage");
    app.stop().await;
  }

  /// A 503 of ours carries `Retry-After` and the stable code `unavailable`.
  fn assert_unavailable(status: &StatusCode, body: &serde_json::Value) {
    assert_eq!(*status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    let body: ErrorBody = serde_json::from_value(body.clone()).unwrap();
    assert_eq!(body.error_code.as_str(), "unavailable");
    assert!(
      body.maybe_retry_after_secs.is_some_and(|secs| secs >= 1),
      "{body:?}"
    );
  }

  async fn start_avatar_upload(
    app: &TestApp,
    user: &teamcraft_server::testing::TestUser,
  ) -> teamcraft_server::testing::Reply {
    app
      .post(
        user,
        "/me/avatar",
        json!({"content_type": "image/png", "size_bytes": 4}),
      )
      .await
  }

  /// The real stack with the database's breaker open: requests that need the database answer at
  /// once, what answers from memory still works, the gateway's handshake says how long to wait, and
  /// the first request after the open time finds Postgres back and closes the breaker.
  #[tokio::test]
  async fn an_open_database_breaker_answers_at_once_and_a_request_closes_it() {
    let clock = ManualClock::new();
    let dependencies = Arc::new(Dependencies::new(
      DependencyOptions::default(),
      Arc::new(clock.clone()),
    ));
    let Some(app) = TestApp::start_with_dependencies(None, None, dependencies.clone()).await else {
      return;
    };
    let carol = member("carol");
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    let database = dependencies.guard(DependencyName::Database);
    for _ in 0..3 {
      database.record_failure();
    }

    let refused = app.get(&carol, "/me").await;
    assert_unavailable(&refused.status, &refused.body);
    let from_memory = app
      .http
      .get(format!("{}/api/v1/server-info", app.base_url))
      .send()
      .await
      .unwrap();
    assert_eq!(from_memory.status(), StatusCode::OK);
    let liveness = app
      .http
      .get(format!("{}/healthz", app.base_url))
      .send()
      .await
      .unwrap();
    assert_eq!(liveness.status(), StatusCode::OK);

    let error = identify_error(&app, &carol).await;
    assert_eq!(error.error_code, ErrorCode::Unavailable);
    assert!(error.maybe_retry_after_secs.is_some(), "{error:?}");

    clock.advance(OPEN_FOR);
    assert_eq!(app.get(&carol, "/me").await.status, StatusCode::OK);
    assert_eq!(database.state(), DependencyState::Ok);
    app.stop().await;
  }

  async fn identify_error(app: &TestApp, user: &teamcraft_server::testing::TestUser) -> ErrorBody {
    let (mut socket, _) = tokio_tungstenite::connect_async(&app.ws_url).await.unwrap();
    let _hello = socket.next().await.unwrap().unwrap();
    let identify = GatewayClientMessage::Identify {
      token: app.token(user),
      device_id: "1".parse().unwrap(),
      activity: DeviceActivity::Active,
    };
    let frame = serde_json::to_string(&identify).unwrap();
    socket.send(Message::text(frame)).await.unwrap();
    let answer = socket.next().await.unwrap().unwrap();
    match serde_json::from_str(answer.to_text().unwrap()).unwrap() {
      GatewayServerMessage::Error { error } => error,
      other => panic!("expected an error frame, got {other:?}"),
    }
  }

  async fn readiness(app: &TestApp) -> HealthResponse {
    let response = app
      .http
      .get(format!("{}/readyz", app.base_url))
      .send()
      .await
      .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
  }

  async fn put(url: &str, length: usize) {
    let response = reqwest::Client::new()
      .put(url)
      .header("content-type", "image/png")
      .body(vec![0_u8; length])
      .send()
      .await
      .unwrap();
    assert!(response.status().is_success());
  }
}

#[cfg(feature = "integration")]
mod clock_skew {
  use serde_json::json;
  use teamcraft_server::testing::TestApp;

  use super::*;
  use crate::common::{call_with_token, member};

  /// The identity provider's clock runs ahead or behind by less than the leeway (30 s): its tokens
  /// work. Far beyond it the token is refused with 401 `invalid_token`, a clear answer the app
  /// handles by refreshing, never a 500.
  #[tokio::test]
  async fn tokens_from_a_skewed_provider_work_within_the_leeway_and_fail_clearly_beyond() {
    let Some(app) = TestApp::start().await else {
      return;
    };
    let carol = member("carol");
    let now = i64::try_from(jsonwebtoken::get_current_timestamp()).unwrap();
    let cases = [
      // Provider 20 s ahead: issued and valid "in the future".
      (json!({"nbf": now + 20}), now + 20, StatusCode::OK),
      // Provider 20 s behind: the token ran out moments ago by our clock.
      (json!({"exp": now - 20}), now - 320, StatusCode::OK),
      // Five minutes ahead and behind: refused, clearly.
      (json!({"nbf": now + 300}), now + 300, StatusCode::UNAUTHORIZED),
      (json!({"exp": now - 300}), now - 600, StatusCode::UNAUTHORIZED),
    ];
    for (overrides, issued_at, expected) in cases {
      let token = app.token_with(&carol, issued_at, &overrides);
      let (status, body) = call_with_token(&app, &token, reqwest::Method::GET, "/me", None).await;
      assert_eq!(status, expected, "{overrides}: {body}");
      if expected == StatusCode::UNAUTHORIZED {
        assert_eq!(body["error_code"], "invalid_token", "{body}");
      }
    }
    app.stop().await;
  }
}

#[cfg(feature = "integration")]
mod push_gateway {
  use teamcraft_proto::{PushKind, PushProvider};
  use teamcraft_server::push::{PushError, Urgency};

  use super::*;
  use crate::push_gateway::{Stack, deliver};

  const APNS_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

  /// A push gateway that answers 503: after three failed pushes the breaker opens and further
  /// pushes go to the durable retry queue (`Unavailable` with the time until the next probe)
  /// without a request; once it is back, the probe delivers and closes the breaker.
  #[tokio::test]
  async fn a_gateway_outage_fails_fast_into_the_retry_queue_and_heals() {
    let stack = Stack::start().await;
    let (guard, clock) = guard(DependencyName::PushGateway);
    stack.client.watch_health(guard.clone());
    stack.fake.set_reply(Some((
      503,
      serde_json::json!({"error_code": "overloaded", "message": "try later"}),
    )));
    for _ in 0..3 {
      let result = push(&stack).await;
      assert!(matches!(result, Err(PushError::Unavailable { .. })), "{result:?}");
    }
    let attempts = stack.fake.push_attempts();
    assert_eq!(guard.state(), DependencyState::Down);

    let fast = push(&stack).await;
    assert!(
      matches!(
        &fast,
        Err(PushError::Unavailable { maybe_retry_after: Some(wait), .. }) if *wait == OPEN_FOR
      ),
      "{fast:?}"
    );
    assert_eq!(
      stack.fake.push_attempts(),
      attempts,
      "nothing was sent while it is down"
    );

    stack.fake.set_reply(None);
    clock.advance(OPEN_FOR);
    push(&stack).await.unwrap();
    assert_eq!(guard.state(), DependencyState::Ok);
  }

  async fn push(stack: &Stack) -> Result<(), PushError> {
    deliver(
      stack,
      PushProvider::Apns,
      APNS_TOKEN,
      PushKind::Message,
      Urgency::Normal,
    )
    .await
  }
}

#[cfg(feature = "integration")]
mod sfu {
  use axum::routing::post;
  use secrecy::SecretString;
  use teamcraft_proto::{CallId, Id, SfuControlAction, SfuControlRequest, Snowflake, UserId};
  use teamcraft_server::config::CallsConfig;
  use teamcraft_server::error::ApiError;
  use teamcraft_server::services::calls::CallService;

  use super::*;
  use crate::calls::SIGNING_KEY_PEM;

  /// The SFU restarts: host controls are retried, then fail fast with 503 while it is down, and go
  /// through again once it is back, without restarting the server.
  #[tokio::test]
  async fn host_controls_fail_fast_while_the_sfu_restarts_and_work_again_after() {
    let (control, url) = FakeControl::start().await;
    let (guard, clock) = guard(DependencyName::Sfu);
    let calls = CallService::new(&calls_config(&url), SIGNING_KEY_PEM.as_bytes())
      .unwrap()
      .with_dependency_guard(guard.clone());

    control.set_down(true);
    for _ in 0..3 {
      let result = calls.send_control(&mute_all()).await;
      assert!(matches!(result, Err(ApiError::Unavailable(_))), "{result:?}");
    }
    assert_eq!(control.requests(), 9, "each control retried twice");
    let fast = calls.send_control(&mute_all()).await;
    assert!(
      matches!(
        &fast,
        Err(ApiError::DependencyDown {
          dependency: DependencyName::Sfu,
          retry_after_secs: 15
        })
      ),
      "{fast:?}"
    );
    assert_eq!(control.requests(), 9);

    control.set_down(false);
    clock.advance(OPEN_FOR);
    calls.send_control(&mute_all()).await.unwrap();
    assert_eq!(guard.state(), DependencyState::Ok);
  }

  fn mute_all() -> SfuControlRequest {
    SfuControlRequest {
      call_id: CallId::from_snowflake(Snowflake::new(7).unwrap()),
      by_user_id: UserId::from_snowflake(Snowflake::new(8).unwrap()),
      action: SfuControlAction::MuteAll,
    }
  }

  fn calls_config(control_url: &str) -> CallsConfig {
    CallsConfig {
      sfu_url: "https://sfu.example.com:4433/call".into(),
      issuer: "https://teamcraft.example.com".into(),
      signing_key_path: "unused.pem".into(),
      join_token_ttl_secs: 60,
      call_idle_secs: 3600,
      maybe_sfu_registration_token: Some(SecretString::from("sfu-token")),
      ring_timeout_secs: 30,
      maybe_sfu_control_url: Some(control_url.to_owned()),
      maybe_sfu_certificate_sha256: None,
      roster_grace_secs: 60,
      roster_timeout_secs: 120,
      max_devices_per_user_per_call: 3,
    }
  }

  /// The SFU's control listener; while "down" it answers 503 like a restarting SFU behind a proxy.
  #[derive(Clone, Default)]
  struct FakeControl {
    is_down: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
  }

  impl FakeControl {
    async fn start() -> (Self, String) {
      let fake = Self::default();
      let app = Router::new()
        .route("/internal/v1/calls/control", post(answer_control))
        .with_state(fake.clone());
      let url = serve(app).await;
      (fake, url)
    }

    fn set_down(&self, is_down: bool) {
      self.is_down.store(is_down, Ordering::SeqCst);
    }

    fn requests(&self) -> usize {
      self.requests.load(Ordering::SeqCst)
    }
  }

  async fn answer_control(State(fake): State<FakeControl>) -> StatusCode {
    fake.requests.fetch_add(1, Ordering::SeqCst);
    if fake.is_down.load(Ordering::SeqCst) {
      StatusCode::SERVICE_UNAVAILABLE
    } else {
      StatusCode::NO_CONTENT
    }
  }
}

mod database {
  use std::future::pending;

  use axum::body::Body;
  use axum::extract::Request;
  use axum::http::header;
  use futures_util::stream;
  use sqlx::postgres::PgPoolOptions;
  use teamcraft_db::{Database, DbError};
  use teamcraft_proto::{ErrorBody, ErrorCode};
  use teamcraft_server::error::ApiError;
  use teamcraft_server::outage_gate::{OutageGate, Ping};
  use tokio::net::TcpListener;
  use tokio::time::Instant;

  use super::*;

  const API_PATH: &str = "/api/v1/me";
  const DEADLINE: Duration = Duration::from_secs(60);
  const READINESS_TIMEOUT: Duration = Duration::from_secs(2);

  mod breaker {
    use super::*;

    /// Postgres gone: each request fails on its own until the third, then none reaches a handler or
    /// the pool, and each answer says when to come back. After the open time one request pings; while
    /// Postgres is still gone it fails and the wait starts over, once Postgres is back it goes on.
    #[tokio::test]
    async fn an_outage_opens_it_and_a_request_probes_it_closed() {
      let rig = Rig::new(Some(DEADLINE));
      rig.fake.set_down(true);
      for _ in 0..3 {
        let response = rig.request(API_PATH).await;
        assert_unavailable(response, DATABASE_RETRY_AFTER).await;
      }
      assert_eq!(rig.fake.handled(), 3);
      assert_eq!(rig.guard.state(), DependencyState::Down);

      let response = rig.request(API_PATH).await;
      assert_unavailable(response, OPEN_FOR).await;
      assert_eq!(
        rig.fake.handled(),
        3,
        "an open breaker keeps requests off the pool"
      );
      assert_eq!(rig.fake.pings(), 0);

      rig.clock.advance(OPEN_FOR);
      let response = rig.request(API_PATH).await;
      assert_unavailable(response, OPEN_FOR).await;
      assert_eq!(rig.fake.pings(), 1, "one probe");
      assert_eq!(rig.fake.handled(), 3);
      assert_eq!(rig.guard.state(), DependencyState::Down);

      rig.fake.set_down(false);
      rig.clock.advance(OPEN_FOR);
      assert_eq!(rig.request(API_PATH).await.status(), StatusCode::OK);
      assert_eq!(rig.fake.pings(), 2);
      assert_eq!(rig.fake.handled(), 4);
      assert_eq!(rig.guard.state(), DependencyState::Ok);
    }

    /// Errors now and then are not an outage: only failures in a row count.
    #[tokio::test]
    async fn answers_in_between_keep_it_closed() {
      let rig = Rig::new(Some(DEADLINE));
      for _ in 0..5 {
        rig.fake.set_down(true);
        rig.request(API_PATH).await;
        rig.request(API_PATH).await;
        rig.fake.set_down(false);
        assert_eq!(rig.request(API_PATH).await.status(), StatusCode::OK);
      }
      assert_eq!(rig.guard.state(), DependencyState::Ok);
    }

    /// A failure that says nothing about Postgres (a refusal, a storage 503) doesn't count.
    #[tokio::test]
    async fn other_errors_do_not_count() {
      let rig = Rig::new(Some(DEADLINE));
      for _ in 0..5 {
        let response = rig
          .gate
          .handle(request(API_PATH), |_| async {
            ApiError::storage_unavailable("attachment storage").into_response()
          })
          .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
      }
      assert_eq!(rig.guard.state(), DependencyState::Ok);
    }

    /// What answers from memory goes on during an outage, and doesn't probe.
    #[tokio::test]
    async fn paths_that_do_not_need_the_database_stay_up() {
      let rig = Rig::new(Some(DEADLINE));
      rig.open();
      for path in [
        "/",
        "/healthz",
        "/api/v1/brand",
        "/api/v1/brand/assets/logo",
        "/api/v1/client-compatibility",
        "/api/v1/openapi.json",
        "/api/v1/server-info",
      ] {
        assert_eq!(rig.request(path).await.status(), StatusCode::OK, "{path}");
      }
      assert_eq!(rig.fake.pings(), 0);
      assert_eq!(rig.guard.state(), DependencyState::Down);
    }

    /// The gateway's handshake uses the database outside the HTTP stack and counts the same.
    #[tokio::test]
    async fn work_outside_the_http_stack_feeds_it_and_is_refused_by_it() {
      let rig = Rig::new(Some(DEADLINE));
      rig.gate.admit().await.unwrap();
      for _ in 0..3 {
        rig.gate.note_outcome::<()>(&Err(ApiError::Database(refused())));
      }
      assert_eq!(rig.guard.state(), DependencyState::Down);
      assert!(rig.gate.admit().await.is_err());
    }

    /// A probe of a Postgres that stopped answering is bounded by the readiness timeout, and meanwhile
    /// only that request waits.
    #[tokio::test(start_paused = true)]
    async fn a_hung_probe_is_bounded_and_others_do_not_wait_for_it() {
      let rig = Rig::new(Some(DEADLINE));
      rig.open();
      rig.fake.set_hung(true);
      rig.clock.advance(OPEN_FOR);
      let started = Instant::now();
      let probe = rig.request(API_PATH);
      let other = async {
        // The probe has taken its turn by the time this one is polled.
        tokio::task::yield_now().await;
        rig.request(API_PATH).await
      };
      let (probe, other) = tokio::join!(probe, other);
      assert_eq!(started.elapsed(), READINESS_TIMEOUT);
      assert_eq!(probe.status(), StatusCode::SERVICE_UNAVAILABLE);
      assert_eq!(other.status(), StatusCode::SERVICE_UNAVAILABLE);
      assert_eq!(rig.fake.pings(), 1);
      assert_eq!(rig.guard.state(), DependencyState::Down);
    }
  }

  mod deadline {
    use super::*;

    /// A query on a connection that stopped answering never returns: the request answers 503 with
    /// `Retry-After` at the deadline instead of hanging.
    #[tokio::test(start_paused = true)]
    async fn a_hung_request_answers_503_at_the_deadline() {
      let rig = Rig::new(Some(DEADLINE));
      let started = Instant::now();
      let response = rig
        .gate
        .handle(request(API_PATH), |_| pending::<Response>())
        .await;
      assert_eq!(started.elapsed(), DEADLINE);
      assert_unavailable(response, DATABASE_RETRY_AFTER).await;
    }

    /// A slow upload that keeps moving is not cut off, however long it takes in total.
    #[tokio::test(start_paused = true)]
    async fn a_body_that_keeps_arriving_is_not_cut_off() {
      let rig = Rig::new(Some(DEADLINE));
      let started = Instant::now();
      let chunks = 10_u32;
      let response = rig
        .gate
        .handle(request_with_body(chunks, Some(DEADLINE / 2)), |request| async {
          let body = axum::body::to_bytes(request.into_body(), usize::MAX)
            .await
            .unwrap();
          assert_eq!(body.len(), chunks as usize);
          StatusCode::OK.into_response()
        })
        .await;
      assert_eq!(response.status(), StatusCode::OK);
      assert!(started.elapsed() > DEADLINE * 2, "{:?}", started.elapsed());
    }

    /// A body that stops is no progress any more: the wait ends a deadline after its last piece.
    #[tokio::test(start_paused = true)]
    async fn a_body_that_stalls_is_cut_off() {
      let rig = Rig::new(Some(DEADLINE));
      let started = Instant::now();
      let response = rig
        .gate
        .handle(request_with_body(2, None), |request| async {
          let _ = axum::body::to_bytes(request.into_body(), usize::MAX).await;
          StatusCode::OK.into_response()
        })
        .await;
      assert_eq!(started.elapsed(), DEADLINE);
      assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test(start_paused = true)]
    async fn imports_and_the_gateway_have_no_deadline() {
      let rig = Rig::new(Some(DEADLINE));
      for path in ["/api/v1/admin/import/users", "/api/v1/gateway"] {
        let waiting = rig.gate.handle(request(path), |_| pending::<Response>());
        assert!(
          tokio::time::timeout(DEADLINE * 10, waiting).await.is_err(),
          "{path}"
        );
      }
    }

    #[tokio::test(start_paused = true)]
    async fn the_deadline_can_be_turned_off() {
      let rig = Rig::new(None);
      let waiting = rig.gate.handle(request(API_PATH), |_| pending::<Response>());
      assert!(tokio::time::timeout(DEADLINE * 10, waiting).await.is_err());
    }
  }

  mod transport {
    use super::*;

    /// Nothing listens on the port: what the pool reports counts as an outage.
    #[tokio::test]
    async fn a_refused_connection_is_an_outage() {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let address = listener.local_addr().unwrap();
      drop(listener);
      let error = dead_database(address).ping().await.unwrap_err();
      assert!(error.is_outage(), "{error}");
    }

    /// The connection opens but nothing ever answers (a black-holed Postgres): bounded by the pool's
    /// acquire timeout, and an outage too.
    #[tokio::test]
    async fn a_silent_server_is_an_outage() {
      let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
      let address = listener.local_addr().unwrap();
      let error = dead_database(address).ping().await.unwrap_err();
      assert!(error.is_outage(), "{error}");
      drop(listener);
    }

    fn dead_database(address: std::net::SocketAddr) -> Database {
      let pool = PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(100))
        .connect_lazy(&format!("postgres://nobody:nothing@{address}/none"))
        .unwrap();
      Database::from_pool(pool)
    }
  }

  const DATABASE_RETRY_AFTER: Duration = Duration::from_secs(5);

  /// The gate on a manual clock, in front of a fake database.
  struct Rig {
    gate: OutageGate,
    guard: Arc<DependencyGuard>,
    clock: ManualClock,
    fake: FakeDatabase,
  }

  impl Rig {
    fn new(maybe_deadline: Option<Duration>) -> Self {
      let (guard, clock) = guard(DependencyName::Database);
      let fake = FakeDatabase::default();
      let gate = OutageGate::new(guard.clone(), fake.ping(), READINESS_TIMEOUT, maybe_deadline);
      Self {
        gate,
        guard,
        clock,
        fake,
      }
    }

    fn open(&self) {
      for _ in 0..3 {
        self.guard.record_failure();
      }
      assert_eq!(self.guard.state(), DependencyState::Down);
    }

    /// One request to a handler that uses the fake database.
    async fn request(&self, path: &str) -> Response {
      let fake = self.fake.clone();
      self
        .gate
        .handle(request(path), |_| async move { fake.handle() })
        .await
    }
  }

  #[derive(Clone, Default)]
  struct FakeDatabase {
    is_down: Arc<AtomicBool>,
    is_hung: Arc<AtomicBool>,
    pings: Arc<AtomicUsize>,
    handled: Arc<AtomicUsize>,
  }

  impl FakeDatabase {
    fn set_down(&self, is_down: bool) {
      self.is_down.store(is_down, Ordering::SeqCst);
    }

    fn set_hung(&self, is_hung: bool) {
      self.is_hung.store(is_hung, Ordering::SeqCst);
    }

    fn pings(&self) -> usize {
      self.pings.load(Ordering::SeqCst)
    }

    fn handled(&self) -> usize {
      self.handled.load(Ordering::SeqCst)
    }

    fn ping(&self) -> Ping {
      let fake = self.clone();
      Arc::new(move || {
        let fake = fake.clone();
        Box::pin(async move {
          fake.pings.fetch_add(1, Ordering::SeqCst);
          if fake.is_hung.load(Ordering::SeqCst) {
            pending::<()>().await;
          }
          if fake.is_down.load(Ordering::SeqCst) {
            Err(refused())
          } else {
            Ok(())
          }
        })
      })
    }

    fn handle(&self) -> Response {
      self.handled.fetch_add(1, Ordering::SeqCst);
      if self.is_down.load(Ordering::SeqCst) {
        ApiError::Database(refused()).into_response()
      } else {
        StatusCode::OK.into_response()
      }
    }
  }

  fn refused() -> DbError {
    DbError::Query(sqlx::Error::Io(std::io::Error::from(
      std::io::ErrorKind::ConnectionRefused,
    )))
  }

  fn request(path: &str) -> Request {
    Request::builder().uri(path).body(Body::empty()).unwrap()
  }

  /// A body of `chunks` bytes, one every `maybe_pause`; without one it stalls after them.
  fn request_with_body(chunks: u32, maybe_pause: Option<Duration>) -> Request {
    let body = stream::unfold(0_u32, move |sent| async move {
      if sent == chunks {
        if maybe_pause.is_none() {
          std::future::pending::<()>().await;
        }
        return None;
      }
      if let Some(pause) = maybe_pause {
        tokio::time::sleep(pause).await;
      }
      Some((Ok::<_, std::io::Error>(Bytes::from_static(b"x")), sent + 1))
    });
    Request::builder()
      .uri(API_PATH)
      .header(header::CONTENT_LENGTH, chunks.to_string())
      .body(Body::from_stream(body))
      .unwrap()
  }

  async fn assert_unavailable(response: Response, retry_after: Duration) {
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let seconds = retry_after.as_secs();
    assert_eq!(
      response.headers()[header::RETRY_AFTER],
      seconds.to_string().as_str()
    );
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
      .await
      .unwrap();
    let body: ErrorBody = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body.error_code, ErrorCode::Unavailable);
    assert_eq!(body.maybe_retry_after_secs, Some(seconds));
  }
}

/// A guard on a manual clock with the default options.
fn guard(name: DependencyName) -> (Arc<DependencyGuard>, ManualClock) {
  let clock = ManualClock::new();
  let guard = Arc::new(DependencyGuard::new(
    name,
    DependencyOptions::default(),
    Arc::new(clock.clone()),
  ));
  (guard, clock)
}

/// A blob store against a fresh fake storage, with its guard on a manual clock.
async fn fake_store() -> (FakeStorage, BlobStore, Arc<DependencyGuard>, ManualClock) {
  let (fake, endpoint) = FakeStorage::start().await;
  let (guard, clock) = guard(DependencyName::ObjectStorage);
  let store = BlobStore::new(&storage_config(&endpoint))
    .unwrap()
    .with_guard(guard.clone());
  (fake, store, guard, clock)
}

fn storage_config(endpoint: &str) -> StorageConfig {
  StorageConfig {
    endpoint: endpoint.to_owned(),
    region: "fake".into(),
    bucket: "bucket".into(),
    access_key: "fake".into(),
    secret_key: SecretString::from("fake-secret"),
    path_style: true,
    presign_expiry_secs: 300,
    max_attachment_bytes: 1024 * 1024,
    multipart_expiry_secs: 3600,
  }
}

/// An S3 stand-in: stores what is put (sizes only), answers HEAD and GET, and on demand fails
/// every request with 503 (an outage), the next few (a hiccup) or refuses them with 403. Signatures
/// are not checked.
#[derive(Clone, Default)]
struct FakeStorage {
  inner: Arc<FakeStorageInner>,
}

#[derive(Default)]
struct FakeStorageInner {
  objects: Mutex<HashMap<String, usize>>,
  requests: AtomicUsize,
  is_failing: AtomicBool,
  is_refusing: AtomicBool,
  failures_left: AtomicUsize,
}

impl FakeStorage {
  async fn start() -> (Self, String) {
    let fake = Self::default();
    let app = Router::new().fallback(answer_storage).with_state(fake.clone());
    let url = serve(app).await;
    (fake, url)
  }

  fn set_failing(&self, is_failing: bool) {
    self.inner.is_failing.store(is_failing, Ordering::SeqCst);
  }

  fn set_refusing(&self, is_refusing: bool) {
    self.inner.is_refusing.store(is_refusing, Ordering::SeqCst);
  }

  fn fail_next(&self, count: usize) {
    self.inner.failures_left.store(count, Ordering::SeqCst);
  }

  fn requests(&self) -> usize {
    self.inner.requests.load(Ordering::SeqCst)
  }

  fn stored(&self, key: &str) -> Option<usize> {
    self
      .inner
      .objects
      .lock()
      .unwrap()
      .get(&format!("/bucket/{key}"))
      .copied()
  }

  fn is_failing_now(&self) -> bool {
    self.inner.is_failing.load(Ordering::SeqCst)
      || self
        .inner
        .failures_left
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| left.checked_sub(1))
        .is_ok()
  }
}

async fn answer_storage(
  State(fake): State<FakeStorage>,
  method: Method,
  uri: Uri,
  _headers: HeaderMap,
  body: Bytes,
) -> Response {
  fake.inner.requests.fetch_add(1, Ordering::SeqCst);
  if fake.is_failing_now() {
    return (
      StatusCode::SERVICE_UNAVAILABLE,
      "<Error><Code>ServiceUnavailable</Code></Error>",
    )
      .into_response();
  }
  if fake.inner.is_refusing.load(Ordering::SeqCst) {
    return (StatusCode::FORBIDDEN, "<Error><Code>AccessDenied</Code></Error>").into_response();
  }
  let key = uri.path().to_owned();
  let mut objects = fake.inner.objects.lock().unwrap();
  match method {
    Method::PUT => {
      objects.insert(key, body.len());
      StatusCode::OK.into_response()
    },
    Method::HEAD | Method::GET => match objects.get(&key) {
      // HEAD answers carry the length of the body they leave out.
      Some(size) => (StatusCode::OK, vec![0_u8; *size]).into_response(),
      None => StatusCode::NOT_FOUND.into_response(),
    },
    Method::DELETE => {
      objects.remove(&key);
      StatusCode::NO_CONTENT.into_response()
    },
    _ => StatusCode::NOT_IMPLEMENTED.into_response(),
  }
}

/// Serves `app` on a free loopback port and returns its base URL.
async fn serve(app: Router) -> String {
  let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
  let address = listener.local_addr().unwrap();
  tokio::spawn(async move {
    axum::serve(listener, app).await.unwrap();
  });
  format!("http://{address}")
}
