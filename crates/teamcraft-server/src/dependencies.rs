//! Health of the backends the server depends on (issue #743): a circuit breaker and short retries
//! per dependency, so an outage of one backend (object storage, identity provider, push gateway,
//! SFU, Postgres) costs fast, clear answers — 503 with `Retry-After` — instead of requests that hang
//! for a timeout each, and `/readyz` reports it without flapping on a single failed probe.
//!
//! The retry arithmetic is the policy the clients use too (`teamcraft_net::retry`). Time comes from
//! a [`Clock`], so tests step it instead of sleeping.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use rand_core::{OsRng, RngCore};
use serde::Deserialize;
use teamcraft_net::retry::{Jitter, RetryPolicy};
use teamcraft_proto::{DependencyHealth, DependencyName, DependencyState};
use tokio::time::Instant;
use tracing::{info, warn};

use crate::auth::BoxFuture;

/// `[dependencies]`: how the server treats a failing backend. One setting for all of them; each
/// keeps its own breaker.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, default)]
pub struct DependenciesConfig {
  /// Failures in a row after which a dependency counts as down: calls fail fast with 503 and
  /// `Retry-After` instead of waiting for their timeout, and `/readyz` reports it.
  pub failure_threshold: u32,
  /// How long a dependency that is down is left alone before one call probes it again.
  pub open_secs: u64,
  /// Extra attempts of an idempotent call that failed on the network or with a 5xx, within one
  /// request (0: none).
  pub retries: u32,
  /// Wait before the first retry; doubles per retry up to `retry_max_ms`, with jitter.
  pub retry_initial_ms: u64,
  pub retry_max_ms: u64,
  /// How long `/readyz` waits for the database to answer; longer counts as a failure.
  pub readiness_timeout_ms: u64,
}

/// The breakers of all dependencies, for `/readyz`.
pub struct Dependencies {
  options: DependencyOptions,
  clock: Arc<dyn Clock>,
  guards: Mutex<Vec<Arc<DependencyGuard>>>,
}

/// The breaker and retries of one dependency.
pub struct DependencyGuard {
  name: DependencyName,
  options: DependencyOptions,
  clock: Arc<dyn Clock>,
  state: Mutex<GuardState>,
}

/// [`DependenciesConfig`] in typed units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DependencyOptions {
  pub failure_threshold: u32,
  pub open_for: Duration,
  pub retries: u32,
  pub retry: RetryPolicy,
  pub readiness_timeout: Duration,
}

/// The time source of the breakers and the retry waits.
pub trait Clock: Send + Sync {
  fn now(&self) -> Instant;
  fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;
}

/// The tokio clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct TokioClock;

/// A call was not made: the dependency is down and its next probe is not due.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{dependency} is unavailable, retry in {} seconds", retry_after.as_secs())]
pub struct CircuitOpen {
  pub dependency: DependencyName,
  /// Until the next probe is due (at least a second, for `Retry-After`).
  pub retry_after: Duration,
}

/// How a failed call reflects on the dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
  /// It is in trouble: unreachable, reset, 5xx. Counts towards the breaker; retried when the call
  /// is idempotent.
  Outage,
  /// It didn't answer in time. Counts towards the breaker but is not retried: a request must not
  /// wait for several timeouts in a row.
  Timeout,
  /// It answered and refused (4xx, not found, a bad token): it works. Counts as a success.
  Refusal,
}

/// Whether a call may be repeated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retries {
  /// Safe to repeat (GET, HEAD, DELETE, PUT of the same object, idempotent control actions).
  Idempotent,
  /// Once only (it creates something each time).
  Once,
}

struct GuardState {
  breaker: Breaker,
  jitter: Jitter,
}

/// Closed (calls go through), open (calls fail fast) or half open (one probe goes through).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Breaker {
  Closed {
    failures: u32,
  },
  Open {
    until: Instant,
  },
  /// A probe is out since `since`; others fail fast. A probe whose request was cancelled never
  /// reports back, so after `open_for` another one may go.
  HalfOpen {
    since: Instant,
  },
}

/// What [`Breaker::admit`] decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
  Allowed,
  Probe,
  Rejected { retry_after: Duration },
}

impl Dependencies {
  pub fn new(options: DependencyOptions, clock: Arc<dyn Clock>) -> Self {
    Self {
      options,
      clock,
      guards: Mutex::new(Vec::new()),
    }
  }

  /// The guard of `name`, created on first use; every caller of one dependency shares it.
  pub fn guard(&self, name: DependencyName) -> Arc<DependencyGuard> {
    let mut guards = lock(&self.guards);
    if let Some(guard) = guards.iter().find(|guard| guard.name == name) {
      return guard.clone();
    }
    let guard = Arc::new(DependencyGuard::new(name, self.options, self.clock.clone()));
    guards.push(guard.clone());
    guard
  }

  /// Each dependency in use and its state.
  pub fn health(&self) -> Vec<DependencyHealth> {
    lock(&self.guards)
      .iter()
      .map(|guard| DependencyHealth {
        dependency: guard.name.clone(),
        state: guard.state(),
      })
      .collect()
  }

  pub fn options(&self) -> DependencyOptions {
    self.options
  }
}

impl Default for Dependencies {
  fn default() -> Self {
    Self::new(DependencyOptions::default(), Arc::new(TokioClock))
  }
}

impl DependencyGuard {
  pub fn new(name: DependencyName, options: DependencyOptions, clock: Arc<dyn Clock>) -> Self {
    Self {
      name,
      options,
      clock,
      state: Mutex::new(GuardState {
        breaker: Breaker::Closed { failures: 0 },
        jitter: Jitter::new(OsRng.next_u64()),
      }),
    }
  }

  /// A guard on its own, outside any registry (tests, tools).
  pub fn standalone(name: DependencyName) -> Arc<Self> {
    Arc::new(Self::new(
      name,
      DependencyOptions::default(),
      Arc::new(TokioClock),
    ))
  }

  pub fn name(&self) -> &DependencyName {
    &self.name
  }

  /// Runs `call` unless the dependency is down, retrying an outage of an idempotent call with the
  /// shared backoff, and records the outcome. `classify` tells an outage from a refusal.
  pub async fn call<T, E, F, Fut>(
    &self,
    retries: Retries,
    classify: impl Fn(&E) -> Fault,
    mut call: F,
  ) -> Result<T, E>
  where
    E: From<CircuitOpen> + std::fmt::Display,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
  {
    let admission = self.admit()?;
    let extra_attempts = match (retries, admission) {
      (Retries::Idempotent, Admission::Allowed) => self.options.retries,
      // A probe is one attempt: a dependency that is still down is not hammered.
      _ => 0,
    };
    let mut failures = 0;
    loop {
      match call().await {
        Ok(value) => {
          self.record_success();
          return Ok(value);
        },
        Err(error) if classify(&error) == Fault::Refusal => {
          self.record_success();
          return Err(error);
        },
        Err(error) if failures < extra_attempts && classify(&error) == Fault::Outage => {
          let wait = self.retry_wait(failures);
          warn!(dependency = %self.name, %error, retry_in_ms = wait.as_millis(), "dependency call failed, retrying");
          self.clock.sleep(wait).await;
          failures += 1;
        },
        Err(error) => {
          self.record_failure();
          return Err(error);
        },
      }
    }
  }

  /// Fails fast while the dependency is down; for callers that hand out work the dependency will
  /// do later (a presigned upload URL), so clients hear "later" from us and not a raw error from it.
  pub fn check(&self) -> Result<(), CircuitOpen> {
    match lock(&self.state).breaker.peek(self.clock.now(), &self.options) {
      Admission::Rejected { retry_after } => Err(self.open(retry_after)),
      Admission::Allowed | Admission::Probe => Ok(()),
    }
  }

  /// The dependency answered (called by code that talks to it without [`Self::call`]).
  pub fn record_success(&self) {
    let mut state = lock(&self.state);
    if !matches!(state.breaker, Breaker::Closed { .. }) {
      info!(dependency = %self.name, "dependency is back");
    }
    state.breaker = Breaker::Closed { failures: 0 };
  }

  /// The dependency failed (called by code that talks to it without [`Self::call`]).
  pub fn record_failure(&self) {
    let now = self.clock.now();
    let mut state = lock(&self.state);
    let before = state.breaker;
    state.breaker = before.after_failure(now, &self.options);
    if matches!(state.breaker, Breaker::Open { .. }) && !matches!(before, Breaker::Open { .. }) {
      warn!(
        dependency = %self.name,
        open_secs = self.options.open_for.as_secs(),
        "dependency is down; calls fail fast until it is probed again"
      );
    }
  }

  /// How the dependency is doing.
  pub fn state(&self) -> DependencyState {
    lock(&self.state).breaker.state(self.clock.now())
  }

  /// `Retry-After` for an answer that blames this dependency: until its next probe, or the
  /// breaker's open time while it still works (a single failure).
  pub fn retry_after(&self) -> Duration {
    match lock(&self.state).breaker {
      Breaker::Open { until } => until.saturating_duration_since(self.clock.now()),
      Breaker::Closed { .. } | Breaker::HalfOpen { .. } => self.options.open_for,
    }
    .max(Duration::from_secs(1))
  }

  fn admit(&self) -> Result<Admission, CircuitOpen> {
    let now = self.clock.now();
    let admission = lock(&self.state).breaker.admit(now, &self.options);
    match admission {
      Admission::Rejected { retry_after } => Err(self.open(retry_after)),
      Admission::Probe => {
        info!(dependency = %self.name, "probing a dependency that was down");
        Ok(admission)
      },
      Admission::Allowed => Ok(admission),
    }
  }

  fn retry_wait(&self, failures: u32) -> Duration {
    let mut state = lock(&self.state);
    self.options.retry.delay(failures, &mut state.jitter, None)
  }

  fn open(&self, retry_after: Duration) -> CircuitOpen {
    CircuitOpen {
      dependency: self.name.clone(),
      retry_after: retry_after.max(Duration::from_secs(1)),
    }
  }
}

impl Breaker {
  /// Decides on a call and moves an open breaker whose time is up to half open.
  fn admit(&mut self, now: Instant, options: &DependencyOptions) -> Admission {
    let admission = self.peek(now, options);
    if admission == Admission::Probe {
      *self = Self::HalfOpen { since: now };
    }
    admission
  }

  /// [`Self::admit`] without taking the probe.
  fn peek(&self, now: Instant, options: &DependencyOptions) -> Admission {
    match *self {
      Self::Closed { .. } => Admission::Allowed,
      Self::Open { until } if now >= until => Admission::Probe,
      Self::Open { until } => Admission::Rejected {
        retry_after: until.saturating_duration_since(now),
      },
      Self::HalfOpen { since } if now.saturating_duration_since(since) >= options.open_for => {
        Admission::Probe
      },
      Self::HalfOpen { since } => Admission::Rejected {
        retry_after: (since + options.open_for).saturating_duration_since(now),
      },
    }
  }

  fn after_failure(self, now: Instant, options: &DependencyOptions) -> Self {
    let open = Self::Open {
      until: now + options.open_for,
    };
    match self {
      Self::Closed { failures } if failures.saturating_add(1) >= options.failure_threshold.max(1) => open,
      Self::Closed { failures } => Self::Closed {
        failures: failures + 1,
      },
      // The probe failed: down for another round.
      Self::HalfOpen { .. } => open,
      // A call admitted before the breaker opened failed late: nothing new.
      Self::Open { until } => Self::Open { until },
    }
  }

  fn state(&self, now: Instant) -> DependencyState {
    match *self {
      Self::Closed { .. } => DependencyState::Ok,
      Self::Open { until } if now < until => DependencyState::Down,
      Self::Open { .. } | Self::HalfOpen { .. } => DependencyState::Recovering,
    }
  }
}

impl Clock for TokioClock {
  fn now(&self) -> Instant {
    Instant::now()
  }

  fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
    Box::pin(tokio::time::sleep(duration))
  }
}

impl Default for DependenciesConfig {
  fn default() -> Self {
    Self {
      failure_threshold: 3,
      open_secs: 15,
      retries: 2,
      retry_initial_ms: 200,
      retry_max_ms: 2_000,
      readiness_timeout_ms: 2_000,
    }
  }
}

impl From<&DependenciesConfig> for DependencyOptions {
  fn from(config: &DependenciesConfig) -> Self {
    Self {
      failure_threshold: config.failure_threshold.max(1),
      open_for: Duration::from_secs(config.open_secs.max(1)),
      retries: config.retries,
      retry: RetryPolicy::new(
        Duration::from_millis(config.retry_initial_ms),
        Duration::from_millis(config.retry_max_ms),
      ),
      readiness_timeout: Duration::from_millis(config.readiness_timeout_ms.max(1)),
    }
  }
}

impl Default for DependencyOptions {
  fn default() -> Self {
    Self::from(&DependenciesConfig::default())
  }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // A panic while holding the lock leaves plain data (counters, instants) that is still valid.
  mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A clock tests step by hand; its sleeps return at once and move it forward.
pub mod testing {
  use std::sync::{Arc, Mutex};
  use std::time::Duration;

  use tokio::time::Instant;

  use super::{Clock, lock};
  use crate::auth::BoxFuture;

  #[derive(Debug, Clone)]
  pub struct ManualClock(Arc<Mutex<Instant>>);

  impl ManualClock {
    pub fn new() -> Self {
      Self(Arc::new(Mutex::new(Instant::now())))
    }

    pub fn advance(&self, duration: Duration) {
      *lock(&self.0) += duration;
    }
  }

  impl Default for ManualClock {
    fn default() -> Self {
      Self::new()
    }
  }

  impl Clock for ManualClock {
    fn now(&self) -> Instant {
      *lock(&self.0)
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
      self.advance(duration);
      Box::pin(std::future::ready(()))
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicU32, Ordering};

  use super::testing::ManualClock;
  use super::*;

  #[derive(Debug, PartialEq, Eq, thiserror::Error)]
  enum FakeError {
    #[error("down")]
    Down,
    #[error("refused")]
    Refused,
    #[error(transparent)]
    Open(#[from] CircuitOpen),
  }

  fn classify(error: &FakeError) -> Fault {
    match error {
      FakeError::Refused => Fault::Refusal,
      FakeError::Down | FakeError::Open(_) => Fault::Outage,
    }
  }

  mod breaker {
    use super::*;

    #[tokio::test]
    async fn it_opens_after_the_threshold_and_then_fails_fast_without_calling() {
      let (guard, clock) = guard(0);
      let calls = AtomicU32::new(0);
      for _ in 0..3 {
        assert_eq!(fail(&guard, &calls).await, Err(FakeError::Down));
      }
      assert_eq!(guard.state(), DependencyState::Down);
      let fast = fail(&guard, &calls).await;
      assert!(
        matches!(&fast, Err(FakeError::Open(open)) if open.retry_after == Duration::from_secs(15)),
        "{fast:?}"
      );
      assert_eq!(calls.load(Ordering::SeqCst), 3, "an open breaker must not call");
      clock.advance(Duration::from_secs(5));
      assert_eq!(guard.retry_after(), Duration::from_secs(10));
      assert!(guard.check().is_err());
    }

    #[tokio::test]
    async fn a_single_failure_does_not_open_it_and_a_success_resets_the_count() {
      let (guard, _clock) = guard(0);
      let calls = AtomicU32::new(0);
      for _ in 0..2 {
        let _ = fail(&guard, &calls).await;
      }
      assert_eq!(guard.state(), DependencyState::Ok);
      succeed(&guard).await.unwrap();
      for _ in 0..2 {
        let _ = fail(&guard, &calls).await;
      }
      assert_eq!(
        guard.state(),
        DependencyState::Ok,
        "the count starts over after a success"
      );
    }

    #[tokio::test]
    async fn after_the_open_time_one_probe_goes_and_a_success_closes_it() {
      let (guard, clock) = guard(0);
      let calls = AtomicU32::new(0);
      open(&guard, &calls).await;
      clock.advance(Duration::from_secs(15));
      assert_eq!(guard.state(), DependencyState::Recovering);
      let probe = guard.admit().unwrap();
      assert_eq!(probe, Admission::Probe);
      // While the probe is out, everything else still fails fast.
      assert!(fail(&guard, &calls).await.is_err());
      assert_eq!(calls.load(Ordering::SeqCst), 3);
      guard.record_success();
      assert_eq!(guard.state(), DependencyState::Ok);
      succeed(&guard).await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_probe_opens_it_for_another_round() {
      let (guard, clock) = guard(0);
      let calls = AtomicU32::new(0);
      open(&guard, &calls).await;
      clock.advance(Duration::from_secs(15));
      assert_eq!(fail(&guard, &calls).await, Err(FakeError::Down));
      assert_eq!(calls.load(Ordering::SeqCst), 4, "the probe called once");
      assert_eq!(guard.state(), DependencyState::Down);
      assert_eq!(guard.retry_after(), Duration::from_secs(15));
    }

    #[tokio::test]
    async fn a_probe_that_never_reports_back_does_not_block_it_for_good() {
      let (guard, clock) = guard(0);
      let calls = AtomicU32::new(0);
      open(&guard, &calls).await;
      clock.advance(Duration::from_secs(15));
      // The probe's request was cancelled: no outcome is recorded.
      assert_eq!(guard.admit().unwrap(), Admission::Probe);
      clock.advance(Duration::from_secs(15));
      assert_eq!(guard.admit().unwrap(), Admission::Probe);
    }

    #[tokio::test]
    async fn refusals_count_as_answers() {
      let (guard, _clock) = guard(0);
      for _ in 0..10 {
        let refused: Result<(), FakeError> = guard
          .call(Retries::Idempotent, classify, || async {
            Err(FakeError::Refused)
          })
          .await;
        assert_eq!(refused, Err(FakeError::Refused));
      }
      assert_eq!(guard.state(), DependencyState::Ok);
    }
  }

  mod retries {
    use super::*;

    #[tokio::test]
    async fn an_idempotent_call_is_retried_with_backoff_and_counts_once() {
      let (guard, clock) = guard(2);
      let started = clock.now();
      let calls = AtomicU32::new(0);
      let result = guard
        .call(Retries::Idempotent, classify, || {
          let attempt = calls.fetch_add(1, Ordering::SeqCst);
          async move {
            if attempt < 2 {
              Err(FakeError::Down)
            } else {
              Ok(attempt)
            }
          }
        })
        .await;
      assert_eq!(result, Ok(2));
      // 200 ms then 400 ms backoff, each at least half of it with equal jitter.
      let waited = clock.now() - started;
      assert!(
        (Duration::from_millis(300)..=Duration::from_millis(600)).contains(&waited),
        "{waited:?}"
      );
      assert_eq!(guard.state(), DependencyState::Ok);
    }

    #[tokio::test]
    async fn a_call_that_is_not_idempotent_is_tried_once() {
      let (guard, _clock) = guard(2);
      let calls = AtomicU32::new(0);
      let result: Result<(), FakeError> = guard
        .call(Retries::Once, classify, || {
          calls.fetch_add(1, Ordering::SeqCst);
          async { Err(FakeError::Down) }
        })
        .await;
      assert_eq!(result, Err(FakeError::Down));
      assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_timeout_is_not_retried_but_counts() {
      let (guard, _clock) = guard(2);
      let calls = AtomicU32::new(0);
      let timeout = |_: &FakeError| Fault::Timeout;
      for _ in 0..3 {
        let result: Result<(), FakeError> = guard
          .call(Retries::Idempotent, timeout, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(FakeError::Down) }
          })
          .await;
        assert_eq!(result, Err(FakeError::Down));
      }
      assert_eq!(calls.load(Ordering::SeqCst), 3, "one attempt per request");
      assert_eq!(guard.state(), DependencyState::Down);
    }

    #[tokio::test]
    async fn exhausted_retries_count_as_one_failure() {
      let (guard, _clock) = guard(2);
      let calls = AtomicU32::new(0);
      let _ = fail(&guard, &calls).await;
      assert_eq!(calls.load(Ordering::SeqCst), 3);
      assert_eq!(
        guard.state(),
        DependencyState::Ok,
        "one failed request is not an outage"
      );
    }
  }

  #[test]
  fn the_registry_shares_one_guard_per_dependency_and_lists_them() {
    let dependencies = Dependencies::new(DependencyOptions::default(), Arc::new(ManualClock::new()));
    let first = dependencies.guard(DependencyName::ObjectStorage);
    let again = dependencies.guard(DependencyName::ObjectStorage);
    assert!(Arc::ptr_eq(&first, &again));
    dependencies.guard(DependencyName::Database);
    let names: Vec<_> = dependencies
      .health()
      .into_iter()
      .map(|health| health.dependency)
      .collect();
    assert_eq!(names, [DependencyName::ObjectStorage, DependencyName::Database]);
  }

  #[test]
  fn the_configuration_has_safe_floors() {
    let options = DependencyOptions::from(&DependenciesConfig {
      failure_threshold: 0,
      open_secs: 0,
      ..DependenciesConfig::default()
    });
    assert_eq!(options.failure_threshold, 1);
    assert_eq!(options.open_for, Duration::from_secs(1));
  }

  fn guard(retries: u32) -> (DependencyGuard, ManualClock) {
    let clock = ManualClock::new();
    let options = DependencyOptions {
      retries,
      ..DependencyOptions::default()
    };
    (
      DependencyGuard::new(DependencyName::ObjectStorage, options, Arc::new(clock.clone())),
      clock,
    )
  }

  async fn fail(guard: &DependencyGuard, calls: &AtomicU32) -> Result<(), FakeError> {
    guard
      .call(Retries::Idempotent, classify, || {
        calls.fetch_add(1, Ordering::SeqCst);
        async { Err(FakeError::Down) }
      })
      .await
  }

  async fn succeed(guard: &DependencyGuard) -> Result<(), FakeError> {
    guard
      .call(Retries::Idempotent, classify, || async { Ok(()) })
      .await
  }

  async fn open(guard: &DependencyGuard, calls: &AtomicU32) {
    for _ in 0..3 {
      let _ = fail(guard, calls).await;
    }
    assert_eq!(guard.state(), DependencyState::Down);
  }
}
