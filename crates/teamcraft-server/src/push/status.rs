//! What the server has seen from each push service since it started, for the admin console
//! (admin.md §3.4). Counters live in memory only: they answer "is it working now", not "since when".

use std::collections::HashMap;
use std::sync::Mutex;

use teamcraft_proto::UnixMillis;

use super::sender::PushError;

/// Longest error text kept per provider.
const MAX_ERROR_CHARS: usize = 200;

/// A snapshot of one provider's delivery results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderHealth {
  pub success_count: u64,
  pub failure_count: u64,
  pub maybe_last_success_at: Option<UnixMillis>,
  pub maybe_last_failure_at: Option<UnixMillis>,
  /// The error of the last failure, cleared by the next success.
  pub maybe_last_error: Option<String>,
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
  /// an invalid token is the device's problem, and a missing sender is not an attempt at all.
  pub(super) fn record(&self, provider: &'static str, result: &Result<(), PushError>, now: UnixMillis) {
    let mut providers = self.lock();
    let health = providers.entry(provider).or_default();
    match result {
      Ok(()) | Err(PushError::Gone) => {
        health.success_count += 1;
        health.maybe_last_success_at = Some(now);
      },
      Err(error @ (PushError::Rejected(_) | PushError::Unavailable(_))) => {
        health.failure_count += 1;
        health.maybe_last_failure_at = Some(now);
        health.maybe_last_error = Some(error.to_string().chars().take(MAX_ERROR_CHARS).collect());
      },
      Err(PushError::NotConfigured | PushError::Invalid(_) | PushError::Crypto(_)) => {},
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<&'static str, ProviderHealth>> {
    // A poisoned lock only means another thread panicked while counting; the counters stay usable.
    self
      .providers
      .lock()
      .unwrap_or_else(|poisoned| poisoned.into_inner())
  }
}
