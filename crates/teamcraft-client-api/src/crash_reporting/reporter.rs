//! The reporter a shell keeps per process: decides whether a report is sent, scrubs and queues
//! it, and hands queued envelopes to a [`CrashTransport`].
//!
//! No timers: shells flush on start, after a capture and when the connection comes back, so an
//! idle app sends nothing (robustness.md, "Network while idle").

use super::event::{AppInfo, CrashEvent, CrashLevel, EventId, InstallationId};
use super::queue::{CrashQueue, CrashQueueLimits, QueuedCrashReport};
use super::scrub::scrub;
use super::{
  CrashReportingConfig, CrashReportingConsent, CrashRequest, CrashSendOutcome, CrashTransport,
  ENVELOPE_CONTENT_TYPE,
};

/// Reports for one app process.
#[derive(Clone, Debug)]
pub struct CrashReporter {
  app: AppInfo,
  installation: InstallationId,
  maybe_config: Option<CrashReportingConfig>,
  consent: CrashReportingConsent,
  limits: CrashQueueLimits,
  queue: CrashQueue,
}

/// What [`CrashReporter::capture`] did with a report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureOutcome {
  Queued,
  /// The instance has no crash reporting, or the user opted out.
  Disabled,
  /// A non-fatal error that the sample rate left out.
  SampledOut,
  /// Larger than the whole queue; dropped.
  TooLarge,
}

/// One envelope taken from the queue for sending; report the result with
/// [`CrashReporter::record_outcome`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSend {
  pub event_id: EventId,
  pub request: CrashRequest,
}

/// What a flush did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FlushSummary {
  pub sent: usize,
  pub dropped: usize,
  /// Reports still queued afterwards.
  pub remaining: usize,
}

impl CrashReporter {
  /// `queue` is what the shell stored last time (or empty).
  pub fn new(app: AppInfo, installation: InstallationId, queue: CrashQueue) -> Self {
    Self {
      app,
      installation,
      maybe_config: None,
      consent: CrashReportingConsent::default(),
      limits: CrashQueueLimits::default(),
      queue,
    }
  }

  /// The instance's setting, from the instance config. `None` turns reports off and drops what
  /// is queued.
  pub fn set_config(&mut self, maybe_config: Option<CrashReportingConfig>) {
    self.maybe_config = maybe_config;
    self.drop_queue_unless_active();
  }

  /// The user's choice; opting out drops what is queued.
  pub fn set_consent(&mut self, consent: CrashReportingConsent) {
    self.consent = consent;
    self.drop_queue_unless_active();
  }

  pub fn is_active(&self) -> bool {
    self.maybe_config.is_some() && self.consent == CrashReportingConsent::Allowed
  }

  /// The queue as it should be stored now.
  pub fn queue(&self) -> &CrashQueue {
    &self.queue
  }

  /// Scrubs and queues a report. `roll` is a uniform random number in `[0, 1)` for sampling.
  pub fn capture(&mut self, event: CrashEvent, roll: f64) -> CaptureOutcome {
    let Some(config) = self.maybe_config.as_ref().filter(|_| self.is_active()) else {
      return CaptureOutcome::Disabled;
    };
    if event.level != CrashLevel::Fatal && !config.sample_rate.keeps(roll) {
      return CaptureOutcome::SampledOut;
    }
    let event = scrub(event);
    let report = QueuedCrashReport {
      event_id: event.event_id,
      created_at_ms: event.timestamp_ms,
      envelope: event.to_envelope(&self.app, &config.environment, self.installation),
      attempts: 0,
      next_attempt_at_ms: event.timestamp_ms,
    };
    if self.queue.push(report, &self.limits) {
      CaptureOutcome::Queued
    } else {
      CaptureOutcome::TooLarge
    }
  }

  /// The next envelope to send, if one is due and reports are on.
  pub fn next_send(&mut self, now_ms: i64) -> Option<PendingSend> {
    let config = self.maybe_config.as_ref().filter(|_| self.is_active())?;
    self.queue.prune(now_ms, &self.limits);
    let report = self.queue.next_due(now_ms)?;
    Some(PendingSend {
      event_id: report.event_id,
      request: CrashRequest {
        url: config.dsn.envelope_url(&self.app.version),
        content_type: ENVELOPE_CONTENT_TYPE,
        body: report.envelope.clone().into_bytes(),
      },
    })
  }

  /// Applies a send's result to the queue. Returns whether the flush should go on: after a
  /// failure or a rate limit it stops, so a dead backend gets one request per flush, not a storm.
  pub fn record_outcome(&mut self, event_id: EventId, outcome: CrashSendOutcome, now_ms: i64) -> bool {
    match outcome {
      CrashSendOutcome::Accepted | CrashSendOutcome::Rejected => {
        self.queue.remove(event_id);
        true
      },
      CrashSendOutcome::RateLimited { retry_after } => {
        self.queue.pause(now_ms, retry_after);
        false
      },
      CrashSendOutcome::Unreachable => {
        self.queue.record_failure(event_id, now_ms, &self.limits);
        false
      },
    }
  }

  /// Sends what is due, one report at a time, for a shell that owns the reporter across the
  /// sends. Shells that share it between tasks use [`Self::next_send`] and
  /// [`Self::record_outcome`] instead, so no lock is held while sending.
  pub async fn flush<T: CrashTransport>(&mut self, transport: &T, now_ms: i64) -> FlushSummary {
    let before = self.queue.len();
    let mut summary = FlushSummary::default();
    while let Some(pending) = self.next_send(now_ms) {
      let outcome = transport.send(pending.request).await;
      if outcome == CrashSendOutcome::Accepted {
        summary.sent += 1;
      }
      if !self.record_outcome(pending.event_id, outcome, now_ms) {
        break;
      }
    }
    summary.remaining = self.queue.len();
    summary.dropped = before.saturating_sub(summary.remaining + summary.sent);
    summary
  }

  fn drop_queue_unless_active(&mut self) {
    if !self.is_active() {
      self.queue.clear();
    }
  }
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::collections::VecDeque;
  use std::future::ready;
  use std::time::Duration;

  use super::super::{AppPlatform, ErrorSource, StackFrame};
  use super::*;

  mod capture {
    use super::*;

    #[test]
    fn needs_a_config_and_consent() {
      let mut reporter = reporter(None);
      assert_eq!(
        reporter.capture(event(0, CrashLevel::Fatal), 0.0),
        CaptureOutcome::Disabled
      );
      reporter.set_config(Some(config(1.0)));
      reporter.set_consent(CrashReportingConsent::OptedOut);
      assert_eq!(
        reporter.capture(event(0, CrashLevel::Fatal), 0.0),
        CaptureOutcome::Disabled
      );
      reporter.set_consent(CrashReportingConsent::Allowed);
      assert_eq!(
        reporter.capture(event(0, CrashLevel::Fatal), 0.0),
        CaptureOutcome::Queued
      );
    }

    #[test]
    fn samples_errors_but_never_crashes() {
      let mut reporter = reporter(Some(config(0.0)));
      assert_eq!(
        reporter.capture(event(0, CrashLevel::Error), 0.5),
        CaptureOutcome::SampledOut
      );
      assert_eq!(
        reporter.capture(event(1, CrashLevel::Fatal), 0.5),
        CaptureOutcome::Queued
      );
    }

    #[test]
    fn opting_out_drops_the_queue() {
      let mut reporter = reporter(Some(config(1.0)));
      reporter.capture(event(0, CrashLevel::Fatal), 0.0);
      reporter.set_consent(CrashReportingConsent::OptedOut);
      assert!(reporter.queue().is_empty());
      reporter.set_consent(CrashReportingConsent::Allowed);
      reporter.capture(event(1, CrashLevel::Fatal), 0.0);
      reporter.set_config(None);
      assert!(reporter.queue().is_empty());
    }

    #[test]
    fn queues_the_scrubbed_envelope() {
      let mut reporter = reporter(Some(config(1.0)));
      let mut leaky = event(0, CrashLevel::Fatal);
      leaky.message = r#"failed for Person { name: "Alice Example" }"#.to_owned();
      reporter.capture(leaky, 0.0);
      let pending = reporter.next_send(0).unwrap();
      let body = String::from_utf8(pending.request.body).unwrap();
      assert!(!body.contains("Alice"));
      assert!(body.contains("failed for Person"));
      assert_eq!(
        pending.request.url,
        "https://key1@errors.example.com/9"
          .parse::<teamcraft_proto::Dsn>()
          .unwrap()
          .envelope_url("1.0.9")
      );
      assert_eq!(pending.request.content_type, ENVELOPE_CONTENT_TYPE);
    }
  }

  mod flush {
    use super::*;

    #[test]
    fn sends_everything_due() {
      let mut reporter = reporter(Some(config(1.0)));
      reporter.capture(event(0, CrashLevel::Fatal), 0.0);
      reporter.capture(event(1, CrashLevel::Fatal), 0.0);
      let transport = FakeTransport::answering([CrashSendOutcome::Accepted, CrashSendOutcome::Accepted]);
      let summary = block_on(reporter.flush(&transport, 0));
      assert_eq!(
        summary,
        FlushSummary {
          sent: 2,
          dropped: 0,
          remaining: 0
        }
      );
    }

    #[test]
    fn a_dead_backend_gets_one_request_per_flush() {
      let mut reporter = reporter(Some(config(1.0)));
      for index in 0..5 {
        reporter.capture(event(index, CrashLevel::Fatal), 0.0);
      }
      let transport = FakeTransport::answering([CrashSendOutcome::Unreachable]);
      let summary = block_on(reporter.flush(&transport, 0));
      assert_eq!(transport.sent(), 1);
      assert_eq!(summary.remaining, 5);
      // The failed report waits for its backoff; the next flush tries the next one.
      let transport = FakeTransport::answering([CrashSendOutcome::Unreachable]);
      block_on(reporter.flush(&transport, 1));
      assert_eq!(transport.sent(), 1);
    }

    #[test]
    fn a_rate_limit_pauses_all_sends() {
      let mut reporter = reporter(Some(config(1.0)));
      reporter.capture(event(0, CrashLevel::Fatal), 0.0);
      reporter.capture(event(1, CrashLevel::Fatal), 0.0);
      let transport = FakeTransport::answering([CrashSendOutcome::RateLimited {
        retry_after: Duration::from_secs(60),
      }]);
      block_on(reporter.flush(&transport, 0));
      assert!(reporter.next_send(59_000).is_none());
      assert!(reporter.next_send(60_000).is_some());
    }

    #[test]
    fn a_refused_report_is_dropped() {
      let mut reporter = reporter(Some(config(1.0)));
      reporter.capture(event(0, CrashLevel::Fatal), 0.0);
      let transport = FakeTransport::answering([CrashSendOutcome::Rejected]);
      let summary = block_on(reporter.flush(&transport, 0));
      assert_eq!(
        summary,
        FlushSummary {
          sent: 0,
          dropped: 1,
          remaining: 0
        }
      );
    }
  }

  struct FakeTransport {
    answers: RefCell<VecDeque<CrashSendOutcome>>,
    sent: RefCell<usize>,
  }

  impl FakeTransport {
    fn answering<const N: usize>(answers: [CrashSendOutcome; N]) -> Self {
      Self {
        answers: RefCell::new(answers.into()),
        sent: RefCell::new(0),
      }
    }

    fn sent(&self) -> usize {
      *self.sent.borrow()
    }
  }

  impl CrashTransport for FakeTransport {
    fn send(&self, _request: CrashRequest) -> impl std::future::Future<Output = CrashSendOutcome> {
      *self.sent.borrow_mut() += 1;
      ready(
        self
          .answers
          .borrow_mut()
          .pop_front()
          .unwrap_or(CrashSendOutcome::Unreachable),
      )
    }
  }

  /// The fake transport's futures are ready at once.
  fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
      std::task::Poll::Ready(output) => output,
      std::task::Poll::Pending => panic!("the fake transport never waits"),
    }
  }

  fn reporter(maybe_config: Option<CrashReportingConfig>) -> CrashReporter {
    let mut reporter = CrashReporter::new(
      AppInfo {
        version: "1.0.9".to_owned(),
        platform: AppPlatform::Desktop,
        os_name: "Linux".to_owned(),
        maybe_os_version: None,
      },
      InstallationId::from_random_bytes([9; 16]),
      CrashQueue::default(),
    );
    reporter.set_config(maybe_config);
    reporter
  }

  fn config(sample_rate: f64) -> CrashReportingConfig {
    CrashReportingConfig::new("https://key1@errors.example.com/9", sample_rate, "production").unwrap()
  }

  fn event(index: u8, level: CrashLevel) -> CrashEvent {
    CrashEvent {
      event_id: EventId::from_random_bytes([index; 16]),
      timestamp_ms: 0,
      level,
      source: ErrorSource::Rust,
      error_type: "panic".to_owned(),
      message: "boom".to_owned(),
      frames: vec![StackFrame::default()],
      breadcrumbs: Vec::new(),
    }
  }
}
