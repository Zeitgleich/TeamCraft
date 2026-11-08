//! Crash and error reports over the Sentry envelope protocol (docs/crash-reports.md, issue #1508).
//!
//! This module builds, scrubs, samples and queues reports; it does no I/O and builds for the
//! browser. The shells install the panic hooks, persist the [`CrashQueue`] and send through a
//! [`CrashTransport`]. Any backend that accepts Sentry envelopes works (GlitchTip is the documented
//! default); the app only knows the instance's [`Dsn`].
//!
//! Privacy (security.md, docs/crash-reports.md "What is never sent"): every report passes
//! [`scrub`] before it is queued, breadcrumbs come only from the redacted debug log ring, and the
//! only identifier is a random [`InstallationId`], never a user ID.

mod dsn;
mod event;
mod panic;
mod queue;
mod reporter;
mod scrub;

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use dsn::{Dsn, DsnError};
pub use event::{
  AppInfo, AppPlatform, CrashEvent, CrashLevel, ErrorSource, EventId, InstallationId, StackFrame,
};
pub use panic::{PanicParts, frames_from_backtrace};
pub use queue::{CrashQueue, CrashQueueLimits, QueuedCrashReport};
pub use reporter::{CaptureOutcome, CrashReporter, FlushSummary, PendingSend};
pub use scrub::scrub;

/// How long one send may take, connect to answer. A dead backend must never hold the app up;
/// the report stays queued and goes with the next flush.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// The `Content-Type` of an envelope request.
pub const ENVELOPE_CONTENT_TYPE: &str = "application/x-sentry-envelope";

/// What the instance's admin set (Administration → Crash reports): where reports go and how many
/// non-fatal errors are sent. Crash reporting is off when the instance has none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CrashReportingConfig {
  pub dsn: Dsn,
  /// Share of non-fatal errors that are sent; crashes are always sent.
  pub sample_rate: SampleRate,
  pub environment: Environment,
}

/// The user's own choice in the privacy settings. Reports are on when the admin enabled them,
/// with a notice on first start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrashReportingConsent {
  #[default]
  Allowed,
  OptedOut,
}

/// A share between 0 and 1.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct SampleRate(f64);

/// The environment name the backend groups reports by (`production`, `staging`): lowercase ASCII
/// letters, digits, `-`, `_` and `.`, 1 to [`Environment::MAX_CHARS`] characters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Environment(String);

/// Sends one envelope. Implementations use [`SEND_TIMEOUT`] and never retry on their own: the
/// queue decides when to try again.
pub trait CrashTransport {
  fn send(&self, request: CrashRequest) -> impl Future<Output = CrashSendOutcome>;
}

/// One HTTP `POST` of an envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrashRequest {
  /// The backend's envelope endpoint, with the DSN's public key as `sentry_key` (public by design:
  /// it only allows sending, and every app of the instance has it).
  pub url: String,
  pub content_type: &'static str,
  pub body: Vec<u8>,
}

/// How a send ended, from the transport's point of view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrashSendOutcome {
  /// The backend took the report (2xx).
  Accepted,
  /// The backend asks to wait (429 or 503 with `Retry-After`); nothing is sent until then.
  RateLimited { retry_after: Duration },
  /// The backend refused this report for good (other 4xx): it is dropped, not retried.
  Rejected,
  /// No answer, a timeout or a server error: the report stays queued with a backoff.
  Unreachable,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CrashReportingError {
  #[error("invalid DSN: {0}")]
  Dsn(#[from] DsnError),
  #[error("the sample rate must be between 0 and 1")]
  SampleRate,
  #[error("the environment name must be 1 to 64 lowercase letters, digits, '-', '_' or '.'")]
  Environment,
}

impl CrashReportingConfig {
  pub fn new(dsn: &str, sample_rate: f64, environment: &str) -> Result<Self, CrashReportingError> {
    Ok(Self {
      dsn: dsn.parse()?,
      sample_rate: SampleRate::new(sample_rate)?,
      environment: Environment::new(environment)?,
    })
  }
}

impl SampleRate {
  pub const ALL: Self = Self(1.0);

  pub fn new(rate: f64) -> Result<Self, CrashReportingError> {
    if (0.0..=1.0).contains(&rate) {
      Ok(Self(rate))
    } else {
      Err(CrashReportingError::SampleRate)
    }
  }

  pub fn get(self) -> f64 {
    self.0
  }

  /// Whether an event with a uniform random `roll` in `[0, 1)` is kept.
  pub fn keeps(self, roll: f64) -> bool {
    roll < self.0
  }
}

impl Default for SampleRate {
  fn default() -> Self {
    Self::ALL
  }
}

impl TryFrom<f64> for SampleRate {
  type Error = CrashReportingError;

  fn try_from(rate: f64) -> Result<Self, Self::Error> {
    Self::new(rate)
  }
}

impl From<SampleRate> for f64 {
  fn from(rate: SampleRate) -> Self {
    rate.0
  }
}

impl Environment {
  pub const MAX_CHARS: usize = 64;

  pub fn new(name: &str) -> Result<Self, CrashReportingError> {
    let is_valid = !name.is_empty()
      && name.len() <= Self::MAX_CHARS
      && name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if is_valid {
      Ok(Self(name.to_owned()))
    } else {
      Err(CrashReportingError::Environment)
    }
  }

  pub fn as_str(&self) -> &str {
    &self.0
  }
}

impl Default for Environment {
  fn default() -> Self {
    Self("production".to_owned())
  }
}

impl TryFrom<String> for Environment {
  type Error = CrashReportingError;

  fn try_from(name: String) -> Result<Self, Self::Error> {
    Self::new(&name)
  }
}

impl From<Environment> for String {
  fn from(environment: Environment) -> Self {
    environment.0
  }
}

impl CrashSendOutcome {
  /// The outcome of an HTTP answer; `maybe_retry_after` is the `Retry-After` header in seconds.
  pub fn from_status(status: u16, maybe_retry_after: Option<&str>) -> Self {
    let retry_after = || {
      Duration::from_secs(
        maybe_retry_after
          .and_then(|value| value.trim().parse::<u64>().ok())
          .unwrap_or(60),
      )
    };
    match status {
      200..=299 => Self::Accepted,
      429 => Self::RateLimited {
        retry_after: retry_after(),
      },
      503 if maybe_retry_after.is_some() => Self::RateLimited {
        retry_after: retry_after(),
      },
      400..=499 => Self::Rejected,
      _ => Self::Unreachable,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn config_validates_its_parts() {
    let config = CrashReportingConfig::new("https://abc123@errors.example.com/7", 0.25, "staging").unwrap();
    assert_eq!(config.sample_rate.get(), 0.25);
    assert_eq!(config.environment.as_str(), "staging");
    assert_eq!(
      CrashReportingConfig::new("https://abc123@errors.example.com/7", 1.5, "prod"),
      Err(CrashReportingError::SampleRate)
    );
    assert_eq!(
      CrashReportingConfig::new("https://abc123@errors.example.com/7", 1.0, "Prod Env"),
      Err(CrashReportingError::Environment)
    );
    assert!(matches!(
      CrashReportingConfig::new("not a dsn", 1.0, "prod"),
      Err(CrashReportingError::Dsn(_))
    ));
  }

  #[test]
  fn config_round_trips_through_json() {
    let config = CrashReportingConfig::new("https://abc123@errors.example.com/7", 0.5, "production").unwrap();
    let json = serde_json::to_value(&config).unwrap();
    assert_eq!(
      json,
      serde_json::json!({
        "dsn": "https://abc123@errors.example.com/7",
        "sample_rate": 0.5,
        "environment": "production",
      })
    );
    assert_eq!(
      serde_json::from_value::<CrashReportingConfig>(json).unwrap(),
      config
    );
    assert!(
      serde_json::from_value::<CrashReportingConfig>(serde_json::json!({
        "dsn": "https://abc123@errors.example.com/7",
        "sample_rate": 2.0,
        "environment": "production",
      }))
      .is_err()
    );
  }

  #[test]
  fn sample_rate_keeps_rolls_below_it() {
    let rate = SampleRate::new(0.1).unwrap();
    assert!(rate.keeps(0.05));
    assert!(!rate.keeps(0.1));
    assert!(!SampleRate::new(0.0).unwrap().keeps(0.0));
    assert!(SampleRate::ALL.keeps(0.999));
  }

  #[test]
  fn send_outcome_follows_the_status() {
    assert_eq!(
      CrashSendOutcome::from_status(200, None),
      CrashSendOutcome::Accepted
    );
    assert_eq!(
      CrashSendOutcome::from_status(429, Some("120")),
      CrashSendOutcome::RateLimited {
        retry_after: Duration::from_secs(120)
      }
    );
    assert_eq!(
      CrashSendOutcome::from_status(429, None),
      CrashSendOutcome::RateLimited {
        retry_after: Duration::from_secs(60)
      }
    );
    assert_eq!(
      CrashSendOutcome::from_status(413, None),
      CrashSendOutcome::Rejected
    );
    assert_eq!(
      CrashSendOutcome::from_status(502, None),
      CrashSendOutcome::Unreachable
    );
    assert_eq!(
      CrashSendOutcome::from_status(503, Some("30")),
      CrashSendOutcome::RateLimited {
        retry_after: Duration::from_secs(30)
      }
    );
  }
}
