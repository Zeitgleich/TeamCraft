//! What the core's threads did while the app measured something (issue #1521, docs/crash-reports.md
//! "Performance data"): every job of the read and engine queues with its wait and run, and the
//! local store's waits for a group of writes. The apps attach the ones that fall into a measure as
//! its steps (`perf_job_spans`), so a slow chat open shows that it waited behind forty chat list
//! reads (#1466), and hear of a job that waited or ran for long (`PerfListener`), which they send
//! as a sampled warning.
//!
//! Only static names leave here: a job's name is its function's name in the code, never data.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use teamcraft_client_api::crash_reporting::trace::{self, ClosedSpan};
use teamcraft_core::timing;

use crate::logging;

/// Jobs kept at most; the oldest drop out. A measure looks back seconds, not minutes.
const MAX_RECORDS: usize = 512;
/// Steps handed to one measure at most: the ones that waited or ran longest.
pub const MAX_SPANS_PER_MEASURE: usize = 24;
/// Jobs shorter than this (wait and run) are not kept: thousands of instant reads explain nothing.
const MIN_KEPT: Duration = Duration::from_millis(2);
/// ... unless the queue was this deep when they joined it: a crowded queue is the story (#1466).
const KEPT_DEPTH: u32 = 4;
/// Default for what counts as a slow job, until the instance's setting arrives.
const DEFAULT_SLOW_MS: u64 = 1_000;
/// Slow jobs reported per process at most, and at most one per job name in this time.
const MAX_SLOW_REPORTS: u32 = 20;
const SLOW_REPORT_GAP: Duration = Duration::from_secs(60);

static RECORDS: Mutex<VecDeque<PerfJobSpan>> = Mutex::new(VecDeque::new());
static READ_DEPTH: AtomicU32 = AtomicU32::new(0);
static ENGINE_DEPTH: AtomicU32 = AtomicU32::new(0);
static SLOW_MS: AtomicU64 = AtomicU64::new(DEFAULT_SLOW_MS);
static SLOW_REPORTS: AtomicU32 = AtomicU32::new(0);
static LISTENER: Mutex<Option<Arc<dyn PerfListener>>> = Mutex::new(None);
static LAST_SLOW: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);
/// Longest query kind a step is named after; anything else is named `unknown`.
const MAX_KIND_LEN: usize = 48;

/// One job, as a step of a measure.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct PerfJobSpan {
  pub kind: PerfJobKind,
  /// The span's operation from the trace layer (`http.client`, `core.step`); empty for a queue job
  /// recorded here (docs/tracing.md). The apps use it as the step's operation when set.
  #[uniffi(default = "")]
  pub op: String,
  /// The job's function in the code (`conversations`, `messages`), or the store's wait.
  pub name: String,
  /// Unix time in milliseconds: queued, started, done. A store wait starts when queued.
  pub enqueued_at_ms: f64,
  pub started_at_ms: f64,
  pub done_at_ms: f64,
  /// Jobs in the queue when this one was queued, itself included.
  pub queue_depth: u32,
  /// CPU time of the job's thread while it ran, where the system tells it (issue #1634): far below
  /// the run, the job waited (a lock, or the process frozen in the background); about the run, it
  /// worked.
  pub maybe_cpu_ms: Option<f64>,
}

/// Which thread a job waited for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum PerfJobKind {
  /// The read thread (local store reads, an action's local half).
  ReadQueue,
  /// The engine thread (network, sync, sending).
  EngineQueue,
  /// The store, while another thread's group of writes held it.
  StoreLock,
  /// A group of writes held the store.
  WriteGroup,
  /// A named step of the core's work (`gateway_resume`, `catch_up_conversation`).
  CoreStep,
}

/// Hears of a job that waited or ran longer than the slow threshold; at most one per job name a
/// minute and a few per process. Called on the core's thread: hand it on, don't block.
#[uniffi::export(with_foreign)]
pub trait PerfListener: Send + Sync {
  fn on_slow_job(&self, job: PerfJobSpan);

  /// The trace layer kept a slow, failed or sampled trace (docs/tracing.md): take it with
  /// `take_trace_envelopes` and hand it to the crash SDK, off this thread.
  fn on_traces_kept(&self);
}

/// A job on its way through a queue.
pub(crate) struct Queued {
  /// The job's span (`queue.read`, `queue.query`, `queue.engine`): made when queued, entered (or
  /// its future instrumented) while it runs, so the trace layer tells waited from ran.
  span: tracing::Span,
  kind: PerfJobKind,
  name: &'static str,
  enqueued_at_ms: f64,
  enqueued: Instant,
  depth: u32,
}

/// A job that started.
pub(crate) struct Running {
  queued: Queued,
  started_at_ms: f64,
  started: Instant,
  maybe_cpu_at_start: Option<Duration>,
}

/// Steps of the measure from `start_ms` to `end_ms` (Unix milliseconds): the queue jobs recorded
/// here and the trace layer's other spans (steps, store waits, requests) that overlap it, the
/// longest first, at most [`MAX_SPANS_PER_MEASURE`], then in time order.
#[uniffi::export]
pub fn perf_job_spans(start_ms: f64, end_ms: f64) -> Vec<PerfJobSpan> {
  let mut records: VecDeque<PerfJobSpan> = RECORDS.lock().unwrap_or_else(PoisonError::into_inner).clone();
  records.extend(
    trace::recent(start_ms, end_ms)
      .iter()
      .filter(|span| !is_recorded_here(span.op))
      .map(job_span_of),
  );
  spans_between(&records, start_ms, end_ms)
}

/// Where slow jobs and kept traces are reported; replaces the one before.
#[uniffi::export]
pub fn set_perf_listener(listener: Arc<dyn PerfListener>) {
  *LISTENER.lock().unwrap_or_else(PoisonError::into_inner) = Some(listener);
  trace::set_kept_hook(on_traces_kept);
}

fn on_traces_kept() {
  let maybe_listener = LISTENER.lock().unwrap_or_else(PoisonError::into_inner).clone();
  if let Some(listener) = maybe_listener {
    listener.on_traces_kept();
  }
}

/// What counts as a slow job, from the instance's setting (`slow_job_ms`); 0 turns reports off.
#[uniffi::export]
pub fn set_slow_job_threshold_ms(threshold_ms: u64) {
  SLOW_MS.store(threshold_ms, Ordering::Relaxed);
}

/// A job of `kind` named by its closure's type (`std::any::type_name`) joins its queue.
pub(crate) fn queued(kind: PerfJobKind, type_name: &'static str) -> Queued {
  queued_on(kind, Lane::Main, type_name)
}

/// [`queued`] on the open chat's read thread (issue #1583): its own span operation, so a wait
/// there is never blamed on the main read thread's jobs.
pub(crate) fn queued_for_query(type_name: &'static str) -> Queued {
  queued_on(PerfJobKind::ReadQueue, Lane::Query, type_name)
}

/// [`queued`] on the actions' thread (issue #1756): an action that waits for the store's write lock
/// is named there, never on a read thread.
pub(crate) fn queued_for_action(type_name: &'static str) -> Queued {
  queued_on(PerfJobKind::ReadQueue, Lane::Act, type_name)
}

/// Which thread of its kind a job runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
  Main,
  Query,
  Act,
}

fn queued_on(kind: PerfJobKind, lane: Lane, type_name: &'static str) -> Queued {
  let depth = depth_of(kind).map_or(1, |depth| depth.fetch_add(1, Ordering::Relaxed) + 1);
  let name = job_name(type_name);
  let span = match (kind, lane) {
    (PerfJobKind::ReadQueue, Lane::Main) => tracing::trace_span!("queue.read", name, depth),
    (PerfJobKind::ReadQueue, Lane::Query) => tracing::trace_span!("queue.query", name, depth),
    (PerfJobKind::ReadQueue, Lane::Act) => tracing::trace_span!("queue.act", name, depth),
    (PerfJobKind::EngineQueue, _) => tracing::trace_span!("queue.engine", name, depth),
    _ => tracing::Span::none(),
  };
  Queued {
    span,
    kind,
    name,
    enqueued_at_ms: now_ms(),
    enqueued: Instant::now(),
    depth,
  }
}

impl Queued {
  /// The job's span: enter it around a job that runs on its thread, or instrument the job's future.
  pub(crate) fn span(&self) -> tracing::Span {
    self.span.clone()
  }

  pub(crate) fn start(self) -> Running {
    if let Some(depth) = depth_of(self.kind) {
      // Saturating: a job queued before this module counted can't make it wrap.
      let _ = depth.try_update(Ordering::Relaxed, Ordering::Relaxed, |depth| {
        Some(depth.saturating_sub(1))
      });
    }
    Running {
      started_at_ms: now_ms(),
      started: Instant::now(),
      maybe_cpu_at_start: logging::thread_cpu_time(),
      queued: self,
    }
  }

  /// The job was skipped (its caller is gone): it leaves the queue without a record.
  pub(crate) fn skip(self) {
    let _ = self.start();
  }
}

impl Running {
  pub(crate) fn done(self) {
    let waited = self.started.saturating_duration_since(self.queued.enqueued);
    let ran = self.started.elapsed();
    if waited + ran < MIN_KEPT && self.queued.depth < KEPT_DEPTH {
      return;
    }
    let span = PerfJobSpan {
      kind: self.queued.kind,
      op: String::new(),
      name: self.queued.name.to_owned(),
      enqueued_at_ms: self.queued.enqueued_at_ms,
      started_at_ms: self.started_at_ms,
      done_at_ms: self.started_at_ms + duration_ms(ran),
      queue_depth: self.queued.depth,
      maybe_cpu_ms: self
        .maybe_cpu_at_start
        .zip(logging::thread_cpu_time())
        .map(|(before, after)| duration_ms(after.saturating_sub(before))),
    };
    keep(span.clone());
    if is_slow(waited, ran) {
      report_slow(self.queued.name, span);
    }
  }
}

/// Ops this module records itself (queue jobs, queries), not taken again from the trace layer.
fn is_recorded_here(op: &str) -> bool {
  op.starts_with("queue.") || op == "db.query"
}

/// A span of the trace layer as a step the apps know.
fn job_span_of(span: &ClosedSpan) -> PerfJobSpan {
  let kind = match span.op {
    "store.lock_wait" => PerfJobKind::StoreLock,
    "store.write_group" => PerfJobKind::WriteGroup,
    _ => PerfJobKind::CoreStep,
  };
  let queued_ms = span.maybe_phases.map_or(0.0, |phases| phases.queued_ms);
  PerfJobSpan {
    kind,
    op: span.op.to_owned(),
    name: span.description.to_owned(),
    enqueued_at_ms: span.start_ms,
    started_at_ms: span.start_ms + queued_ms,
    done_at_ms: span.end_ms,
    queue_depth: 0,
    maybe_cpu_ms: span.maybe_cpu_ms,
  }
}

/// A query of the app's answered on the read thread (issue #1634): kept as a step named after its
/// kind (`query_feed_extras`), and reported when slow, so a slow `query` job says which one it
/// was. `kind` is the query's tag from the app's code; anything else is named `unknown`.
pub(crate) fn query_ran(kind: &str, started_at_ms: f64, ran: Duration) {
  if ran < MIN_KEPT {
    return;
  }
  let is_code_name = !kind.is_empty()
    && kind.len() <= MAX_KIND_LEN
    && kind
      .bytes()
      .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
  let name = format!("query_{}", if is_code_name { kind } else { "unknown" });
  let span = PerfJobSpan {
    kind: PerfJobKind::CoreStep,
    op: "db.query".to_owned(),
    name: name.clone(),
    enqueued_at_ms: started_at_ms,
    started_at_ms,
    done_at_ms: started_at_ms + duration_ms(ran),
    queue_depth: 0,
    maybe_cpu_ms: None,
  };
  keep(span.clone());
  if is_slow(Duration::ZERO, ran) {
    report_slow(&name, span);
  }
}

fn keep(span: PerfJobSpan) {
  let mut records = RECORDS.lock().unwrap_or_else(PoisonError::into_inner);
  if records.len() >= MAX_RECORDS {
    records.pop_front();
  }
  records.push_back(span);
}

fn spans_between(records: &VecDeque<PerfJobSpan>, start_ms: f64, end_ms: f64) -> Vec<PerfJobSpan> {
  let mut spans: Vec<PerfJobSpan> = records
    .iter()
    .filter(|span| span.done_at_ms >= start_ms && span.enqueued_at_ms <= end_ms)
    .cloned()
    .collect();
  spans.sort_by(|a, b| (b.done_at_ms - b.enqueued_at_ms).total_cmp(&(a.done_at_ms - a.enqueued_at_ms)));
  spans.truncate(MAX_SPANS_PER_MEASURE);
  spans.sort_by(|a, b| a.enqueued_at_ms.total_cmp(&b.enqueued_at_ms));
  spans
}

fn is_slow(waited: Duration, ran: Duration) -> bool {
  let threshold_ms = SLOW_MS.load(Ordering::Relaxed);
  threshold_ms > 0 && (duration_ms(waited) >= threshold_ms as f64 || duration_ms(ran) >= threshold_ms as f64)
}

fn report_slow(name: &str, span: PerfJobSpan) {
  let now = Instant::now();
  {
    let mut last = LAST_SLOW.lock().unwrap_or_else(PoisonError::into_inner);
    let last = last.get_or_insert_with(HashMap::new);
    if last
      .get(name)
      .is_some_and(|at| now.saturating_duration_since(*at) < SLOW_REPORT_GAP)
    {
      return;
    }
    if SLOW_REPORTS.fetch_add(1, Ordering::Relaxed) >= MAX_SLOW_REPORTS {
      return;
    }
    last.insert(name.to_owned(), now);
  }
  let maybe_listener = LISTENER.lock().unwrap_or_else(PoisonError::into_inner).clone();
  if let Some(listener) = maybe_listener {
    listener.on_slow_job(span);
  }
}

fn depth_of(kind: PerfJobKind) -> Option<&'static AtomicU32> {
  match kind {
    PerfJobKind::ReadQueue => Some(&READ_DEPTH),
    PerfJobKind::EngineQueue => Some(&ENGINE_DEPTH),
    PerfJobKind::StoreLock | PerfJobKind::WriteGroup | PerfJobKind::CoreStep => None,
  }
}

fn job_name(type_name: &'static str) -> &'static str {
  timing::code_name(type_name)
}

pub(crate) fn now_ms() -> f64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1_000.0)
}

fn duration_ms(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_measure_gets_the_overlapping_jobs_longest_first_bounded() {
    let mut records = VecDeque::new();
    for index in 0..40 {
      records.push_back(span(f64::from(index) * 10.0, 5.0));
    }
    // The chat open's read waited behind all of them.
    records.push_back(span(0.0, 6_000.0));
    let spans = spans_between(&records, 100.0, 7_000.0);
    assert_eq!(spans.len(), MAX_SPANS_PER_MEASURE);
    assert!(
      spans.iter().any(|span| span.done_at_ms == 6_000.0),
      "the longest is kept"
    );
    assert!(
      spans
        .windows(2)
        .all(|pair| pair[0].enqueued_at_ms <= pair[1].enqueued_at_ms)
    );
    assert!(spans_between(&records, 10_000.0, 11_000.0).is_empty());
  }

  #[test]
  fn a_query_is_named_by_its_kind_and_only_by_code_names() {
    let started = now_ms() + 1.0e9;
    query_ran("feed_extras", started, Duration::from_millis(30));
    query_ran("Robert's files", started, Duration::from_millis(30));
    query_ran("threads", started, Duration::ZERO);
    let names: Vec<String> = perf_job_spans(started, started + 100.0)
      .into_iter()
      .map(|span| span.name)
      .collect();
    assert!(names.contains(&"query_feed_extras".to_owned()));
    assert!(names.contains(&"query_unknown".to_owned()), "never data");
    assert!(!names.contains(&"query_threads".to_owned()), "too short to keep");
  }

  #[test]
  fn queue_depth_counts_what_waits() {
    let first = queued(PerfJobKind::EngineQueue, "x::first::{{closure}}");
    let second = queued(PerfJobKind::EngineQueue, "x::second::{{closure}}");
    assert!(second.depth > first.depth);
    first.start().done();
    second.skip();
  }

  #[test]
  fn the_threshold_decides_what_is_slow() {
    set_slow_job_threshold_ms(1_000);
    assert!(is_slow(Duration::from_millis(1_200), Duration::ZERO));
    assert!(is_slow(Duration::ZERO, Duration::from_secs(2)));
    assert!(!is_slow(Duration::from_millis(999), Duration::from_millis(999)));
    set_slow_job_threshold_ms(0);
    assert!(!is_slow(Duration::from_secs(9), Duration::ZERO));
    set_slow_job_threshold_ms(DEFAULT_SLOW_MS);
  }

  fn span(enqueued: f64, took: f64) -> PerfJobSpan {
    PerfJobSpan {
      kind: PerfJobKind::ReadQueue,
      op: String::new(),
      name: "conversations".to_owned(),
      enqueued_at_ms: enqueued,
      started_at_ms: enqueued,
      done_at_ms: enqueued + took,
      queue_depth: 1,
      maybe_cpu_ms: None,
    }
  }
}
