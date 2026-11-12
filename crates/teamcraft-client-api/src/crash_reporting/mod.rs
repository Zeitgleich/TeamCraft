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

mod call;
mod event;
mod panic;
mod queue;
mod reporter;
mod script_stack;
mod scrub;
pub mod trace;
mod trace_fields;
mod transaction;

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  CrashReportsInfo, DEFAULT_CALL_REPORTS_PER_CALL, DEFAULT_SLOW_JOB_MS, is_crash_environment,
};
pub use teamcraft_proto::{Dsn, DsnError};
use thiserror::Error;

pub use call::{
  BrowserFamily, BrowserInfo, CallContext, CallEventDetail, CallProblem, CallQualitySummary, CallRef,
  CallReport, CallReportGate, CallTransportKind, MAX_CALL_TRAIL, MAX_RECEIVED_TRACKS, ReceivedVideoContext,
  SentVideoContext, Unverified, identifier,
};
pub use event::{
  AppInfo, AppPlatform, CrashEvent, CrashLevel, ErrorSource, EventId, InstallationId, StackFrame,
};
pub use panic::{MAX_PENDING_PANICS, PanicParts, PendingPanic, frames_from_backtrace};
pub use queue::{CrashQueue, CrashQueueLimits, QueuedCrashReport};
pub use reporter::{
  CaptureOutcome, CrashReporter, FlushSummary, MAX_TRANSACTIONS_PER_FLUSH, PendingSend, SendKind,
  TRANSACTION_BATCH,
};
pub use script_stack::frames_from_script_stack;
pub use scrub::{scrub, without_ids};
pub use transaction::{
  MAX_MEASUREMENTS, MAX_SPANS, MAX_TAGS, PerfOperation, PerfSpan, PerfStatus, PerfUnit, SpanId, TraceId,
  Transaction,
};

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
  /// Share of measured operations sent as performance data (issue #1521). Absent from a stored
  /// config of an older version: none, until the server's setting comes in.
  #[serde(default = "SampleRate::none")]
  pub trace_rate: SampleRate,
  /// A job of the core that waits or runs this long, in milliseconds, is reported (mobile apps);
  /// 0: never.
  #[serde(default = "default_slow_job_ms")]
  pub slow_job_ms: u32,
  /// Problem reports one call may send at most (docs/crash-reports.md, "Call problems"); 0: none.
  #[serde(default = "default_call_reports_per_call")]
  pub call_reports_per_call: u8,
  pub environment: Environment,
  /// The user's opt-out counts; off: the instance requires reports.
  #[serde(default = "is_on")]
  pub allows_opt_out: bool,
  /// The apps say once, on the first start with reports on, that they send them.
  #[serde(default = "is_on")]
  pub shows_notice: bool,
}

/// The user's own choice in the privacy settings. Reports are on when the admin enabled them,
/// with a notice on first start; it counts only while the instance allows opting out.
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
/// letters, digits, `-`, `_` and `.`, 1 to 64 characters.
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
      trace_rate: SampleRate::NONE,
      slow_job_ms: DEFAULT_SLOW_JOB_MS,
      call_reports_per_call: DEFAULT_CALL_REPORTS_PER_CALL,
      environment: Environment::new(environment)?,
      allows_opt_out: true,
      shows_notice: true,
    })
  }

  /// From what `GET /api/v1/server-info` said; a server's value that doesn't check is ignored
  /// (reports stay off) rather than trusted.
  pub fn from_info(info: &CrashReportsInfo) -> Result<Self, CrashReportingError> {
    Ok(Self {
      allows_opt_out: info.allows_opt_out,
      shows_notice: info.shows_notice,
      trace_rate: SampleRate::new(f64::from(info.trace_percent) / 100.0)?,
      slow_job_ms: info.slow_job_ms,
      call_reports_per_call: info.call_reports_per_call,
      ..Self::new(
        &info.dsn,
        f64::from(info.sample_percent) / 100.0,
        &info.environment,
      )?
    })
  }
}

fn is_on() -> bool {
  true
}

fn default_slow_job_ms() -> u32 {
  DEFAULT_SLOW_JOB_MS
}

fn default_call_reports_per_call() -> u8 {
  DEFAULT_CALL_REPORTS_PER_CALL
}

impl SampleRate {
  pub const ALL: Self = Self(1.0);
  pub const NONE: Self = Self(0.0);

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

  fn none() -> Self {
    Self::NONE
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
  pub fn new(name: &str) -> Result<Self, CrashReportingError> {
    if is_crash_environment(name) {
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
  fn config_from_the_server_info() {
    let info = CrashReportsInfo {
      dsn: "https://abc123@errors.example.com/7".to_owned(),
      sample_percent: 20,
      trace_percent: 5,
      slow_job_ms: 1_000,
      call_reports_per_call: 3,
      environment: "production".to_owned(),
      allows_opt_out: false,
      shows_notice: true,
    };
    let config = CrashReportingConfig::from_info(&info).unwrap();
    assert_eq!(config.sample_rate.get(), 0.2);
    assert_eq!(config.trace_rate.get(), 0.05);
    assert!(!config.allows_opt_out);
    assert_eq!(config.call_reports_per_call, 3);
    let hostile = CrashReportsInfo {
      sample_percent: 200,
      ..info
    };
    assert_eq!(
      CrashReportingConfig::from_info(&hostile),
      Err(CrashReportingError::SampleRate)
    );
    let hostile = CrashReportsInfo {
      sample_percent: 20,
      trace_percent: 101,
      ..hostile
    };
    assert_eq!(
      CrashReportingConfig::from_info(&hostile),
      Err(CrashReportingError::SampleRate)
    );
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
        "trace_rate": 0.0,
        "slow_job_ms": 1000,
        "call_reports_per_call": 8,
        "environment": "production",
        "allows_opt_out": true,
        "shows_notice": true,
      })
    );
    assert_eq!(
      serde_json::from_value::<CrashReportingConfig>(json).unwrap(),
      config
    );
    let older: CrashReportingConfig = serde_json::from_value(serde_json::json!({
      "dsn": "https://abc123@errors.example.com/7",
      "sample_rate": 0.5,
      "environment": "production",
    }))
    .unwrap();
    assert_eq!(older.trace_rate, SampleRate::NONE);
    assert_eq!(older.call_reports_per_call, DEFAULT_CALL_REPORTS_PER_CALL);
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
