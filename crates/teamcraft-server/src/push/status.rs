//! What the server has seen from each push service since it started, for the admin console
//! (admin.md §3.4). Counters live in memory only: they answer "is it working now", not "since when".

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use teamcraft_proto::UnixMillis;

use super::sender::PushError;

/// Longest error text kept per provider.
const MAX_ERROR_CHARS: usize = 200;
/// How long a provider that refused a padded wake-up as too large gets small ones at once, before
/// the padded size is tried again (its limit may have been raised).
const COMPACT_FOR: Duration = Duration::from_secs(60 * 60);

/// A snapshot of one provider's delivery results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderHealth {
  pub success_count: u64,
  pub failure_count: u64,
  pub maybe_last_success_at: Option<UnixMillis>,
  pub maybe_last_failure_at: Option<UnixMillis>,
  /// The error of the last failure, cleared by the next success.
  pub maybe_last_error: Option<String>,
  /// Web Push and UnifiedPush: sends that failed at an endpoint a user registered. Users choose
  /// those endpoints, so their failures are only counted; they never make the provider failing.
  pub endpoint_failure_count: u64,
  /// Padded wake-ups the push service refused as too large (a gateway configured below 2048 sealed
  /// bytes); each was sent again small, without the message.
  pub too_large_count: u64,
  pub maybe_last_too_large_at: Option<UnixMillis>,
}

impl ProviderHealth {
  /// The most recent attempt that said anything about the push service failed.
  pub fn is_failing(&self) -> bool {
    match (self.maybe_last_failure_at, self.maybe_last_success_at) {
      (Some(failure), Some(success)) => failure >= success,
      (Some(_), None) => true,
      _ => false,
    }
  }
}

#[derive(Default)]
pub(super) struct ProviderHealthTracker {
  providers: Mutex<HashMap<&'static str, ProviderHealth>>,
}

impl ProviderHealthTracker {
  pub(super) fn snapshot(&self, provider: &'static str) -> ProviderHealth {
    self.lock().get(provider).cloned().unwrap_or_default()
  }

  /// Counts the result. Only outcomes that reflect the push service count: a dead registration or
  /// an invalid token is the device's problem, and a missing sender is not an attempt at all. For
  /// endpoints users registered (`is_user_endpoint`), a failure is the endpoint's, not the
  /// provider's: it is counted apart and leaves the provider's state and last error alone.
  pub(super) fn record(
    &self,
    provider: &'static str,
    is_user_endpoint: bool,
    result: &Result<(), PushError>,
    now: UnixMillis,
  ) {
    let mut providers = self.lock();
    let health = providers.entry(provider).or_default();
    match result {
      Ok(()) | Err(PushError::Gone) => {
        health.success_count += 1;
        health.maybe_last_success_at = Some(now);
      },
      Err(PushError::Rejected(_) | PushError::Unavailable { .. } | PushError::TooLarge)
        if is_user_endpoint =>
      {
        health.endpoint_failure_count += 1;
      },
      Err(error @ (PushError::Rejected(_) | PushError::Unavailable { .. } | PushError::TooLarge)) => {
        health.failure_count += 1;
        health.maybe_last_failure_at = Some(now);
        health.maybe_last_error = Some(error.to_string().chars().take(MAX_ERROR_CHARS).collect());
      },
      Err(PushError::NotConfigured | PushError::Invalid(_) | PushError::Crypto(_)) => {},
    }
  }

  /// Notes that the provider refused a padded wake-up as too large.
  pub(super) fn record_too_large(&self, provider: &'static str, now: UnixMillis) {
    let mut providers = self.lock();
    let health = providers.entry(provider).or_default();
    health.too_large_count += 1;
    health.maybe_last_too_large_at = Some(now);
  }

  /// Whether wake-ups for the provider go out small at once: it refused a padded one within
  /// [`COMPACT_FOR`].
  pub(super) fn sends_compact(&self, provider: &'static str, now: UnixMillis) -> bool {
    let window = i64::try_from(COMPACT_FOR.as_millis()).unwrap_or(i64::MAX);
    self
      .lock()
      .get(provider)
      .and_then(|health| health.maybe_last_too_large_at)
      .is_some_and(|at| now.0.saturating_sub(at.0) < window)
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<&'static str, ProviderHealth>> {
    // A poisoned lock only means another thread panicked while counting; the counters stay usable.
    self
      .providers
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn failures_at_user_endpoints_never_make_the_provider_failing() {
    let tracker = ProviderHealthTracker::default();
    tracker.record("webpush", true, &Err(PushError::Rejected(403)), UnixMillis(2));
    tracker.record(
      "webpush",
      true,
      &Err(PushError::unavailable("timeout")),
      UnixMillis(3),
    );
    let health = tracker.snapshot("webpush");
    assert_eq!(health.endpoint_failure_count, 2);
    assert_eq!(health.failure_count, 0);
    assert_eq!(health.maybe_last_error, None);
    assert!(!health.is_failing());
  }

  #[test]
  fn a_refusal_as_too_large_sends_small_for_an_hour() {
    let tracker = ProviderHealthTracker::default();
    assert!(!tracker.sends_compact("apns", UnixMillis(0)));
    tracker.record_too_large("apns", UnixMillis(1_000));
    assert!(tracker.sends_compact("apns", UnixMillis(1_000 + 59 * 60 * 1000)));
    assert!(!tracker.sends_compact("apns", UnixMillis(1_000 + 60 * 60 * 1000)));
    assert!(!tracker.sends_compact("fcm", UnixMillis(1_000)));
    assert_eq!(tracker.snapshot("apns").too_large_count, 1);
  }

  #[test]
  fn failures_of_operator_push_services_do() {
    let tracker = ProviderHealthTracker::default();
    tracker.record("fcm", false, &Err(PushError::Rejected(401)), UnixMillis(2));
    let health = tracker.snapshot("fcm");
    assert_eq!((health.failure_count, health.endpoint_failure_count), (1, 0));
    assert!(health.is_failing());
    assert!(health.maybe_last_error.is_some());
  }
}
