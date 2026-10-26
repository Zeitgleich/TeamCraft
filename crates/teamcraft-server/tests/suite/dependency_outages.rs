//! Fault injection against fakes (issue #743): object storage, push gateway and SFU control fail on
//! demand, and the tests check that the server degrades gracefully — retries, then a breaker that
//! fails fast with 503 and `Retry-After`, readiness that reports it — and recovers by itself once
//! the dependency is back. Time is a manual clock the tests step; nothing sleeps.
//!
//! The storage cases at the store level need nothing else; the HTTP, push and SFU cases need the
//! dev Postgres (`--features integration`). The heavier cases against the real binaries (Postgres
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
  use serde_json::json;
  use teamcraft_proto::{ErrorBody, HealthResponse, HealthStatus};
  use teamcraft_server::testing::TestApp;

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
