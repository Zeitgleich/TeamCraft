//! Counters in the Prometheus text format. Aggregates only: no server ID, token or wake-up is ever
//! a label, so scraping reveals nothing about who sends.

use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};

use teamcraft_proto::PushProvider;

const PREFIX: &str = "teamcraft_push_gateway";

/// What happened to a push that reached the forwarding step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
  Accepted,
  TokenGone,
  Rejected,
  Unavailable,
  Invalid,
}

/// Why a request was turned away before it was forwarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
  Malformed,
  Unauthorized,
  UnknownServer,
  Blocked,
  Replay,
  RateLimited,
  QuotaExceeded,
  TokenLimited,
  NotConfigured,
}

const PROVIDERS: [&str; 2] = ["apns", "fcm"];
const RESULTS: [(PushResult, &str); 5] = [
  (PushResult::Accepted, "accepted"),
  (PushResult::TokenGone, "token_gone"),
  (PushResult::Rejected, "rejected"),
  (PushResult::Unavailable, "unavailable"),
  (PushResult::Invalid, "invalid"),
];
const REFUSALS: [(Refusal, &str); 9] = [
  (Refusal::Malformed, "malformed"),
  (Refusal::Unauthorized, "unauthorized"),
  (Refusal::UnknownServer, "unknown_server"),
  (Refusal::Blocked, "blocked"),
  (Refusal::Replay, "replay"),
  (Refusal::RateLimited, "rate_limited"),
  (Refusal::QuotaExceeded, "quota_exceeded"),
  (Refusal::TokenLimited, "token_limited"),
  (Refusal::NotConfigured, "provider_not_configured"),
];

#[derive(Default)]
pub struct Metrics {
  pushes: [[AtomicU64; RESULTS.len()]; PROVIDERS.len()],
  refusals: [AtomicU64; REFUSALS.len()],
  registrations: AtomicU64,
}

impl Metrics {
  pub fn count_push(&self, provider: &PushProvider, result: PushResult) {
    if let Some(row) = provider_index(provider).map(|index| &self.pushes[index]) {
      row[result as usize].fetch_add(1, Ordering::Relaxed);
    }
  }

  pub fn count_refusal(&self, refusal: Refusal) {
    self.refusals[refusal as usize].fetch_add(1, Ordering::Relaxed);
  }

  pub fn count_registration(&self) {
    self.registrations.fetch_add(1, Ordering::Relaxed);
  }

  /// The exposition text; `servers` is the number of registered servers.
  pub fn render(&self, servers: u64) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# TYPE {PREFIX}_pushes_total counter");
    for (index, provider) in PROVIDERS.iter().enumerate() {
      for (position, (_, result)) in RESULTS.iter().enumerate() {
        let value = self.pushes[index][position].load(Ordering::Relaxed);
        let _ = writeln!(
          out,
          "{PREFIX}_pushes_total{{provider=\"{provider}\",result=\"{result}\"}} {value}"
        );
      }
    }
    let _ = writeln!(out, "# TYPE {PREFIX}_refused_total counter");
    for (position, (_, reason)) in REFUSALS.iter().enumerate() {
      let value = self.refusals[position].load(Ordering::Relaxed);
      let _ = writeln!(out, "{PREFIX}_refused_total{{reason=\"{reason}\"}} {value}");
    }
    let _ = writeln!(out, "# TYPE {PREFIX}_registrations_total counter");
    let _ = writeln!(
      out,
      "{PREFIX}_registrations_total {}",
      self.registrations.load(Ordering::Relaxed)
    );
    let _ = writeln!(out, "# TYPE {PREFIX}_servers gauge");
    let _ = writeln!(out, "{PREFIX}_servers {servers}");
    out
  }
}

fn provider_index(provider: &PushProvider) -> Option<usize> {
  match provider {
    PushProvider::Apns => Some(0),
    PushProvider::Fcm => Some(1),
    _ => None,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn counters_render_without_identifying_labels() {
    let metrics = Metrics::default();
    metrics.count_push(&PushProvider::Apns, PushResult::Accepted);
    metrics.count_push(&PushProvider::Fcm, PushResult::TokenGone);
    metrics.count_push(&PushProvider::WebPush, PushResult::Accepted);
    metrics.count_refusal(Refusal::Blocked);
    metrics.count_registration();
    let text = metrics.render(3);
    assert!(text.contains("pushes_total{provider=\"apns\",result=\"accepted\"} 1"));
    assert!(text.contains("pushes_total{provider=\"fcm\",result=\"token_gone\"} 1"));
    assert!(text.contains("refused_total{reason=\"blocked\"} 1"));
    assert!(text.contains("registrations_total 1"));
    assert!(text.contains("_servers 3"));
  }
}
