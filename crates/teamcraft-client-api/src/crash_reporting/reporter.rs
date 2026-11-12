//! The reporter a shell keeps per process: decides whether a report is sent, scrubs and queues
//! it, and hands queued envelopes to a [`CrashTransport`].
//!
//! No timers: shells flush on start, after a capture and when the connection comes back, so an
//! idle app sends nothing (robustness.md, "Network while idle").
//!
//! Performance data (issue #1521) goes into a second queue with its own bounds, so a busy app's
//! transactions never push a crash report out; a flush sends crash reports first and at most
//! [`MAX_TRANSACTIONS_PER_FLUSH`] transactions.

use super::event::{AppInfo, CrashEvent, CrashLevel, EventId, InstallationId};
use super::queue::{CrashQueue, CrashQueueLimits, QueuedCrashReport};
use super::scrub::scrub;
use super::trace::{TraceSettings, TraceTree};
use super::transaction::Transaction;
use super::{
  CrashReportingConfig, CrashReportingConsent, CrashRequest, CrashSendOutcome, CrashTransport,
  ENVELOPE_CONTENT_TYPE, SampleRate,
};

/// Transactions sent per flush at most: one request each (the protocol takes one per envelope),
/// so a flush stays short and the relay's per-address rate limit has room for crash reports.
pub const MAX_TRANSACTIONS_PER_FLUSH: usize = 5;
/// Queued transactions from which a shell flushes without waiting for one of the usual triggers:
/// a busy session sends its data in small batches, an idle one sends nothing.
pub const TRANSACTION_BATCH: usize = 5;

/// Reports for one app process.
#[derive(Clone, Debug)]
pub struct CrashReporter {
  app: AppInfo,
  installation: InstallationId,
  maybe_config: Option<CrashReportingConfig>,
  consent: CrashReportingConsent,
  limits: CrashQueueLimits,
  queue: CrashQueue,
  transaction_limits: CrashQueueLimits,
  transactions: CrashQueue,
  /// Transactions the current flush may still send ([`Self::begin_flush`]).
  transactions_left: usize,
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
  pub kind: SendKind,
  pub request: CrashRequest,
}

/// Which queue an envelope came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendKind {
  CrashReport,
  Transaction,
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
      transaction_limits: CrashQueueLimits::transactions(),
      transactions: CrashQueue::default(),
      transactions_left: 0,
    }
  }

  /// The performance data the shell stored last time.
  pub fn with_transactions(mut self, transactions: CrashQueue) -> Self {
    self.transactions = transactions;
    self.drop_queue_unless_active();
    self
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

  /// On when the instance has reports on and the user didn't opt out, or the instance doesn't
  /// allow opting out.
  pub fn is_active(&self) -> bool {
    self
      .maybe_config
      .as_ref()
      .is_some_and(|config| self.consent == CrashReportingConsent::Allowed || !config.allows_opt_out)
  }

  /// The instance's setting in force, for the privacy switch and the first-start notice.
  pub fn config(&self) -> Option<&CrashReportingConfig> {
    self.maybe_config.as_ref()
  }

  /// The queue as it should be stored now.
  pub fn queue(&self) -> &CrashQueue {
    &self.queue
  }

  /// The performance data queue as it should be stored now.
  pub fn transactions(&self) -> &CrashQueue {
    &self.transactions
  }

  /// Whether an operation starting now is measured: reports are on and the trace rate keeps
  /// `roll` (uniform in `[0, 1)`). Decided at the start, so an unsampled operation costs nothing.
  pub fn should_trace(&self, roll: f64) -> bool {
    self
      .maybe_config
      .as_ref()
      .filter(|_| self.is_active())
      .is_some_and(|config| config.trace_rate.keeps(roll))
  }

  /// What the trace layer collects now (docs/tracing.md): on while reports are on and the instance
  /// keeps performance data at all (a trace rate above 0).
  pub fn trace_settings(&self) -> TraceSettings {
    match self.maybe_config.as_ref().filter(|_| self.is_active()) {
      Some(config) => TraceSettings {
        is_on: config.trace_rate.get() > 0.0,
        trace_rate: config.trace_rate,
        slow_ms: config.slow_job_ms,
      },
      None => TraceSettings {
        is_on: false,
        trace_rate: SampleRate::NONE,
        slow_ms: 0,
      },
    }
  }

  /// Queues the trees the trace layer kept as transactions; `random` gives 16 bytes from the
  /// platform's secure random source per tree.
  pub fn capture_traces(&mut self, trees: &[TraceTree], mut random: impl FnMut() -> [u8; 16]) {
    for tree in trees {
      // A full queue or reports turned off only mean this tree isn't sent.
      let _ = self.capture_transaction(&Transaction::from_trace(tree, random()));
    }
  }

  /// Queues a finished transaction (sampled with [`Self::should_trace`] when it started). It needs
  /// no scrubbing: it has no free text (transaction.rs).
  pub fn capture_transaction(&mut self, transaction: &Transaction) -> CaptureOutcome {
    let Some(config) = self.maybe_config.as_ref().filter(|_| self.is_active()) else {
      return CaptureOutcome::Disabled;
    };
    if config.trace_rate.get() <= 0.0 {
      return CaptureOutcome::SampledOut;
    }
    let created_at_ms = transaction_time_ms(transaction);
    let report = QueuedCrashReport {
      event_id: transaction.event_id(),
      created_at_ms,
      envelope: transaction.to_envelope(&self.app, &config.environment, self.installation),
      attempts: 0,
      next_attempt_at_ms: created_at_ms,
    };
    if self.transactions.push(report, &self.transaction_limits) {
      CaptureOutcome::Queued
    } else {
      CaptureOutcome::TooLarge
    }
  }

  /// Whether enough performance data is queued to flush without another trigger.
  pub fn has_transaction_batch(&self) -> bool {
    self.transactions.len() >= TRANSACTION_BATCH
  }

  /// Starts a flush: it may send up to [`MAX_TRANSACTIONS_PER_FLUSH`] transactions after the
  /// crash reports. Without it [`Self::next_send`] hands out crash reports only.
  pub fn begin_flush(&mut self) {
    self.transactions_left = MAX_TRANSACTIONS_PER_FLUSH;
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

  /// The next envelope to send, if one is due and reports are on: crash reports first, then
  /// performance data while the flush has room for it.
  pub fn next_send(&mut self, now_ms: i64) -> Option<PendingSend> {
    let config = self.maybe_config.as_ref().filter(|_| self.is_active())?;
    self.queue.prune(now_ms, &self.limits);
    self.transactions.prune(now_ms, &self.transaction_limits);
    let (kind, report) = match self.queue.next_due(now_ms) {
      Some(report) => (SendKind::CrashReport, report),
      None if self.transactions_left > 0 => (SendKind::Transaction, self.transactions.next_due(now_ms)?),
      None => return None,
    };
    let pending = PendingSend {
      event_id: report.event_id,
      kind,
      request: CrashRequest {
        url: config.dsn.envelope_url(&self.app.version),
        content_type: ENVELOPE_CONTENT_TYPE,
        body: report.envelope.clone().into_bytes(),
      },
    };
    if kind == SendKind::Transaction {
      self.transactions_left -= 1;
    }
    Some(pending)
  }

  /// Applies a send's result to the queue. Returns whether the flush should go on: after a
  /// failure or a rate limit it stops, so a dead backend gets one request per flush, not a storm.
  pub fn record_outcome(&mut self, event_id: EventId, outcome: CrashSendOutcome, now_ms: i64) -> bool {
    let (queue, limits) = if self.queue.contains(event_id) {
      (&mut self.queue, &self.limits)
    } else {
      (&mut self.transactions, &self.transaction_limits)
    };
    match outcome {
      CrashSendOutcome::Accepted | CrashSendOutcome::Rejected => {
        queue.remove(event_id);
        true
      },
      CrashSendOutcome::RateLimited { retry_after } => {
        // The backend (or the relay) limits the sender, not one kind of item.
        self.queue.pause(now_ms, retry_after);
        self.transactions.pause(now_ms, retry_after);
        false
      },
      CrashSendOutcome::Unreachable => {
        queue.record_failure(event_id, now_ms, limits);
        false
      },
    }
  }

  /// Sends what is due, one report at a time, for a shell that owns the reporter across the
  /// sends. Shells that share it between tasks use [`Self::next_send`] and
  /// [`Self::record_outcome`] instead, so no lock is held while sending.
  pub async fn flush<T: CrashTransport>(&mut self, transport: &T, now_ms: i64) -> FlushSummary {
    self.begin_flush();
    let before = self.queue.len() + self.transactions.len();
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
    summary.remaining = self.queue.len() + self.transactions.len();
    summary.dropped = before.saturating_sub(summary.remaining + summary.sent);
    summary
  }

  fn drop_queue_unless_active(&mut self) {
    if !self.is_active() {
      self.queue.clear();
      self.transactions.clear();
    }
  }
}

/// When a transaction ended, whole milliseconds, for the queue's age and backoff.
fn transaction_time_ms(transaction: &Transaction) -> i64 {
  // Precision: a time in milliseconds is far inside i64; the fraction doesn't matter here.
  transaction.end_ms() as i64
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
    fn a_mandatory_instance_ignores_the_opt_out() {
      let mut mandatory = config(1.0);
      mandatory.allows_opt_out = false;
      let mut reporter = reporter(Some(mandatory));
      reporter.set_consent(CrashReportingConsent::OptedOut);
      assert!(reporter.is_active());
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

  mod transactions {
    use super::super::super::{PerfOperation, PerfStatus, SampleRate, Transaction};
    use super::*;

    #[test]
    fn sampling_follows_the_trace_rate_and_the_consent() {
      let mut reporter = reporter(Some(traced(0.25)));
      assert!(reporter.should_trace(0.1));
      assert!(!reporter.should_trace(0.25));
      reporter.set_consent(CrashReportingConsent::OptedOut);
      assert!(!reporter.should_trace(0.0));
      assert_eq!(
        reporter.capture_transaction(&transaction(0)),
        CaptureOutcome::Disabled
      );
      let mut untraced = reporter_with(config(1.0));
      assert!(
        !untraced.should_trace(0.0),
        "an older server's config traces nothing"
      );
      assert_eq!(
        untraced.capture_transaction(&transaction(0)),
        CaptureOutcome::SampledOut
      );
    }

    #[test]
    fn crash_reports_go_first_and_a_flush_sends_a_bounded_number() {
      let mut reporter = reporter(Some(traced(1.0)));
      for index in 0..(MAX_TRANSACTIONS_PER_FLUSH as u8 + 3) {
        assert_eq!(
          reporter.capture_transaction(&transaction(100 + index)),
          CaptureOutcome::Queued
        );
      }
      assert!(reporter.has_transaction_batch());
      reporter.capture(event(0, CrashLevel::Fatal), 0.0);
      assert!(
        reporter
          .next_send(0)
          .is_some_and(|pending| pending.kind == SendKind::CrashReport),
        "crash reports go even outside a flush"
      );
      let transport = FakeTransport::answering([CrashSendOutcome::Accepted; 20]);
      let summary = block_on(reporter.flush(&transport, 0));
      assert_eq!(summary.sent, 1 + MAX_TRANSACTIONS_PER_FLUSH);
      assert_eq!(summary.remaining, 3);
      assert!(
        reporter.next_send(0).is_none(),
        "transactions wait for the next flush"
      );
    }

    #[test]
    fn transactions_never_push_a_crash_report_out() {
      let mut reporter = reporter(Some(traced(1.0)));
      reporter.capture(event(0, CrashLevel::Fatal), 0.0);
      for index in 0..200 {
        reporter.capture_transaction(&transaction(index));
      }
      assert_eq!(reporter.queue().len(), 1);
      assert_eq!(
        reporter.transactions().len(),
        CrashQueueLimits::transactions().max_reports
      );
    }

    #[test]
    fn a_rate_limit_on_a_transaction_pauses_crash_reports_too() {
      let mut reporter = reporter(Some(traced(1.0)));
      reporter.capture_transaction(&transaction(1));
      let transport = FakeTransport::answering([CrashSendOutcome::RateLimited {
        retry_after: Duration::from_secs(60),
      }]);
      block_on(reporter.flush(&transport, 0));
      reporter.capture(event(0, CrashLevel::Fatal), 0.0);
      reporter.begin_flush();
      assert!(reporter.next_send(59_000).is_none());
      assert!(reporter.next_send(60_000).is_some());
    }

    #[test]
    fn a_failed_transaction_backs_off_in_its_own_queue() {
      let mut reporter = reporter(Some(traced(1.0)));
      reporter.capture_transaction(&transaction(1));
      let transport = FakeTransport::answering([CrashSendOutcome::Unreachable]);
      block_on(reporter.flush(&transport, 0));
      assert_eq!(reporter.transactions().len(), 1);
      reporter.capture(event(0, CrashLevel::Fatal), 0.0);
      assert!(
        reporter
          .next_send(1)
          .is_some_and(|pending| pending.kind == SendKind::CrashReport),
        "a crash report doesn't wait for a transaction's backoff"
      );
    }

    #[test]
    fn opting_out_drops_queued_transactions_and_a_stored_buffer_is_restored() {
      let mut reporter = reporter(Some(traced(1.0)));
      reporter.capture_transaction(&transaction(1));
      let stored = reporter.transactions().clone();
      reporter.set_consent(CrashReportingConsent::OptedOut);
      assert!(reporter.transactions().is_empty());
      let restored = reporter_with(traced(1.0)).with_transactions(stored);
      assert_eq!(restored.transactions().len(), 1);
    }

    #[test]
    fn the_envelope_is_a_transaction() {
      let mut reporter = reporter(Some(traced(1.0)));
      reporter.capture_transaction(&transaction(1));
      reporter.begin_flush();
      let pending = reporter.next_send(0).unwrap();
      assert_eq!(pending.kind, SendKind::Transaction);
      let body = String::from_utf8(pending.request.body).unwrap();
      assert!(body.lines().nth(1).unwrap().contains(r#""type":"transaction""#));
      assert!(body.contains(r#""transaction":"channel.switch""#));
    }

    fn traced(rate: f64) -> CrashReportingConfig {
      CrashReportingConfig {
        trace_rate: SampleRate::new(rate).unwrap(),
        ..config(1.0)
      }
    }

    fn reporter_with(config: CrashReportingConfig) -> CrashReporter {
      reporter(Some(config))
    }

    fn transaction(index: u8) -> Transaction {
      Transaction::start(PerfOperation::ChannelSwitch, -42.0, [index; 40]).finish(0.0, PerfStatus::Ok)
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
      maybe_call: None,
    }
  }
}
