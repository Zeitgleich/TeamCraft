//! Fault injection against the real server binary (issue #743): the object storage and the
//! identity provider cut off or hanging behind a relay, Postgres cut off while readiness is probed.
//! Each test asserts that the server stays up, answers what it can't do with 503, `Retry-After`
//! and the stable code `unavailable` (never a raw error, never a sign-out), reports the dependency
//! in `/readyz`, and recovers without a restart once the dependency is back. Server restarts with
//! connected clients, gateway drops and resume are in `server_robustness.rs`; the breakers' timing
//! is tested on a manual clock in the server's own suite (`dependency_outages.rs` there).

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde_json::json;
use teamcraft_e2e::rest::Reply;
use teamcraft_e2e::{Stack, UserTokens};
use teamcraft_proto::{DependencyName, DependencyState, HealthResponse, HealthStatus};

/// Breakers probe again after a second, so recovery is quick to observe.
const OPEN_SECS: &str = "1";
/// How long recovery may take once the dependency is back.
const RECOVERY: Duration = Duration::from_secs(30);

mod storage {
  use super::*;

  /// The storage goes away: completing an upload answers 503 with `Retry-After`; after a few
  /// failures new upload URLs are refused at once and `/readyz` says `degraded` (still 200, the
  /// storage is down); once the storage is back, an upload goes through without a restart.
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_storage_outage_answers_503_with_retry_after_and_heals_without_a_restart() {
    let Some(mut stack) = storage_stack().await else {
      return;
    };
    let alice = stack.tokens("alice");
    stack.user_id(&alice).await;
    start_upload(&stack, &alice).await.expect(StatusCode::CREATED);

    stack.storage_relay().cut();
    for _ in 0..3 {
      assert_unavailable(&complete_upload(&stack, &alice).await);
    }
    let started = Instant::now();
    let refused = start_upload(&stack, &alice).await;
    assert_unavailable(&refused);
    assert!(
      started.elapsed() < Duration::from_secs(1),
      "a refusal while the storage is down must be immediate, took {:?}",
      started.elapsed()
    );
    let ready = readiness(&stack).await;
    assert_eq!(ready.status, HealthStatus::Degraded, "{ready:?}");
    assert_eq!(
      state_of(&ready, DependencyName::ObjectStorage),
      Some(DependencyState::Down)
    );
    assert!(stack.server().is_running());

    stack.storage_relay().restore();
    let upload = eventually(|| start_upload(&stack, &alice), StatusCode::CREATED).await;
    put(&stack, upload.body["upload_url"].as_str().unwrap(), 4).await;
    complete_upload(&stack, &alice).await.expect(StatusCode::OK);
    assert_eq!(readiness(&stack).await.status, HealthStatus::Ok);
    stack.stop().await;
  }

  /// A storage that accepts connections and never answers: the request costs one timeout (timeouts
  /// are not repeated), not one per retry, and still answers 503 with `Retry-After`.
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn a_hanging_storage_costs_one_timeout_and_answers_503() {
    let Some(mut stack) = storage_stack().await else {
      return;
    };
    let alice = stack.tokens("alice");
    stack.user_id(&alice).await;
    start_upload(&stack, &alice).await.expect(StatusCode::CREATED);

    stack.storage_relay().stall();
    let started = Instant::now();
    let reply = complete_upload(&stack, &alice).await;
    let took = started.elapsed();
    eprintln!("PERF storage hanging: answered after {took:?}");
    assert_unavailable(&reply);
    // The storage client's timeout is 10 s; three of them would be 30 s.
    assert!(took < Duration::from_secs(20), "took {took:?}");

    stack.storage_relay().restore();
    eventually(|| start_upload(&stack, &alice), StatusCode::CREATED).await;
    stack.stop().await;
  }

  async fn storage_stack() -> Option<Stack> {
    Stack::builder()
      .with_storage_relay()
      .with_env("TEAMCRAFT_DEPENDENCIES__OPEN_SECS", OPEN_SECS)
      .start()
      .await
  }

  async fn start_upload(stack: &Stack, user: &UserTokens) -> Reply {
    stack
      .post(
        user,
        "/me/avatar",
        json!({"content_type": "image/png", "size_bytes": 4}),
      )
      .await
  }

  async fn complete_upload(stack: &Stack, user: &UserTokens) -> Reply {
    stack.post(user, "/me/avatar/complete", json!({})).await
  }

  async fn put(stack: &Stack, url: &str, length: usize) {
    let response = stack
      .http
      .put(url)
      .header("content-type", "image/png")
      .body(vec![0_u8; length])
      .send()
      .await
      .unwrap();
    assert!(response.status().is_success(), "{}", response.status());
  }
}

mod identity_provider {
  use super::*;

  /// The provider's keys can't be fetched before the server has any: requests answer 503 with
  /// `Retry-After` (never 401, so no client signs its user out), `/readyz` shows the provider
  /// down after a few attempts, and everything works once it is back.
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn without_keys_requests_answer_503_not_401_and_heal() {
    let Some(mut stack) = identity_stack().await else {
      return;
    };
    let alice = stack.tokens("alice");
    stack.identity_relay().cut();
    for _ in 0..3 {
      assert_unavailable(&stack.get(&alice, "/me").await);
      // The cache asks the provider at most once a second.
      tokio::time::sleep(Duration::from_millis(1100)).await;
    }
    let ready = readiness(&stack).await;
    // Down, or already due for its next probe (the breaker opens for a second here).
    assert!(
      matches!(
        state_of(&ready, DependencyName::IdentityProvider),
        Some(DependencyState::Down | DependencyState::Recovering)
      ),
      "{ready:?}"
    );
    assert_eq!(ready.status, HealthStatus::Degraded);

    stack.identity_relay().restore();
    eventually(|| stack.get(&alice, "/me"), StatusCode::OK).await;
    stack.stop().await;
  }

  /// Once the keys are cached, a provider outage changes nothing for people already signed in.
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn with_cached_keys_an_outage_keeps_everyone_signed_in() {
    let Some(mut stack) = identity_stack().await else {
      return;
    };
    let alice = stack.tokens("alice");
    stack.get(&alice, "/me").await.expect(StatusCode::OK);
    stack.identity_relay().cut();
    for _ in 0..5 {
      stack.get(&alice, "/me").await.expect(StatusCode::OK);
    }
    stack.stop().await;
  }

  async fn identity_stack() -> Option<Stack> {
    Stack::builder()
      .with_identity_relay()
      .with_env("TEAMCRAFT_OIDC__JWKS_MIN_REFRESH_INTERVAL_SECS", "1")
      .with_env("TEAMCRAFT_DEPENDENCIES__OPEN_SECS", OPEN_SECS)
      .start()
      .await
  }
}

mod database {
  use super::*;

  /// Postgres cut off: readiness turns 503 after a few failed probes (not on the first slow one),
  /// each probe answers within its timeout instead of hanging, a request answers 503 with
  /// `Retry-After`, and readiness comes back by itself once Postgres does.
  #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
  async fn readiness_follows_the_database_without_flapping_and_heals() {
    let Some(mut stack) = Stack::builder()
      .with_database_relay()
      .with_env("TEAMCRAFT_DEPENDENCIES__OPEN_SECS", OPEN_SECS)
      .start()
      .await
    else {
      return;
    };
    let alice = stack.tokens("alice");
    stack.user_id(&alice).await;

    stack.database_relay().cut();
    let reply = stack.get(&alice, "/me").await;
    assert_unavailable(&reply);
    let mut probes = 0;
    loop {
      probes += 1;
      let started = Instant::now();
      let response = stack
        .http
        .get(format!("{}/readyz", stack.base_url))
        .send()
        .await
        .unwrap();
      assert!(
        started.elapsed() < Duration::from_secs(4),
        "a readiness probe hung for {:?}",
        started.elapsed()
      );
      if response.status() == StatusCode::SERVICE_UNAVAILABLE {
        break;
      }
      assert!(probes < 3, "readiness still OK after {probes} failed probes");
    }
    eprintln!("readiness reported the outage after {probes} probes");
    assert!(stack.server().is_running());

    stack.database_relay().restore();
    let deadline = Instant::now() + RECOVERY;
    while readiness_status(&stack).await != StatusCode::OK {
      assert!(Instant::now() < deadline, "readiness did not come back");
      tokio::time::sleep(Duration::from_millis(250)).await;
    }
    stack.get(&alice, "/me").await.expect(StatusCode::OK);
    stack.stop().await;
  }
}

/// A 503 of ours: the stable code `unavailable` and a `Retry-After` in the body.
fn assert_unavailable(reply: &Reply) {
  assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE, "{}", reply.body);
  assert_eq!(reply.error_code(), "unavailable", "{}", reply.body);
  assert!(
    reply.body["retry_after_secs"]
      .as_u64()
      .is_some_and(|secs| secs >= 1),
    "no retry_after_secs: {}",
    reply.body
  );
}

async fn readiness(stack: &Stack) -> HealthResponse {
  let response = stack
    .http
    .get(format!("{}/readyz", stack.base_url))
    .send()
    .await
    .unwrap();
  response.json().await.unwrap()
}

async fn readiness_status(stack: &Stack) -> StatusCode {
  stack
    .http
    .get(format!("{}/readyz", stack.base_url))
    .send()
    .await
    .map_or(StatusCode::SERVICE_UNAVAILABLE, |response| response.status())
}

fn state_of(ready: &HealthResponse, dependency: DependencyName) -> Option<DependencyState> {
  ready
    .dependencies
    .iter()
    .find(|health| health.dependency == dependency)
    .map(|health| health.state.clone())
}

/// Repeats `request` until it answers `expected`, for at most [`RECOVERY`].
async fn eventually<F, Fut>(mut request: F, expected: StatusCode) -> Reply
where
  F: FnMut() -> Fut,
  Fut: std::future::Future<Output = Reply>,
{
  let deadline = Instant::now() + RECOVERY;
  loop {
    let reply = request().await;
    if reply.status == expected {
      return reply;
    }
    assert!(
      Instant::now() < deadline,
      "still {} after {RECOVERY:?}: {}",
      reply.status,
      reply.body
    );
    tokio::time::sleep(Duration::from_millis(250)).await;
  }
}
