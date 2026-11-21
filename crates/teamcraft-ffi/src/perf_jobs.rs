//! What the core's threads did while the app measured something (issue #1521, docs/crash-reports.md
//! "Performance data"): every job of the read and engine queues with its wait and run, and the
//! local store's waits for a group of writes. The apps attach the ones that fall into a measure as
//! its steps (`perf_job_spans`), so a slow chat open shows that it waited behind forty chat list
//! reads (#1466), and hear of a job that waited or ran for long (`PerfListener`), which they send
//! as a sampled warning.
//!
//! A stall is reported once, by its cause (issue #1759): a job is slow when it *ran* long; a job
//! that only waited is not reported as its own, but as "lane blocked" naming the longest job that
//! held its thread meanwhile, grouped by that job.
//!
//! A job that did not run is not reported (issue #2146): the OS freezes a backgrounded app (Doze,
//! the cached-app freezer, iOS suspension) in the middle of a job, and the job's wall time then
//! counts minutes in which nothing happened. A period counts as frozen when the app was in the
//! background during it, or the wall clock ran at least [`MIN_CLOCK_GAP`] ahead of the monotonic
//! clock (the device slept), *and* the thread's CPU time, where the system tells it, was far below
//! the period ([`FROZEN_CPU_SHARE`]). A job that waited on a lock in the foreground is no freeze
//! and is still reported; its `maybe_cpu_ms` far below its run says it waited, not worked.
//!
//! Only static names leave here: a job's name is its function's name in the code, never data.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use teamcraft_client_api::crash_reporting::trace::{self, ClosedSpan};
use teamcraft_core::timing;

use crate::logging;
use crate::stall_watch::{self, PROCESS_STALL};

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
/// Jobs that held a thread for at least this long are remembered as possible blockers.
const MIN_BLOCKER: Duration = MIN_KEPT;
/// Blockers remembered at most.
const MAX_LANE_RUNS: usize = 128;
/// Slow jobs reported per process at most, and at most one per job name in this time.
const MAX_SLOW_REPORTS: u32 = 20;
const SLOW_REPORT_GAP: Duration = Duration::from_secs(60);
/// The wall clock running this far ahead of the monotonic one means the device slept meanwhile.
const MIN_CLOCK_GAP: Duration = Duration::from_secs(1);
/// A frozen period used less than this share of its length as CPU time.
const FROZEN_CPU_SHARE: f64 = 0.1;
/// A job's wait or run that was at least this share inside a stall of the whole process is
/// reported as blocked by the stall (issue #2225).
const STALLED_SHARE: f64 = 0.5;

static RECORDS: Mutex<VecDeque<PerfJobSpan>> = Mutex::new(VecDeque::new());
static READ_DEPTH: AtomicU32 = AtomicU32::new(0);
static ENGINE_DEPTH: AtomicU32 = AtomicU32::new(0);
static SLOW_MS: AtomicU64 = AtomicU64::new(DEFAULT_SLOW_MS);
static SLOW_REPORTS: AtomicU32 = AtomicU32::new(0);
static LANE_DEPTH: [AtomicU32; 4] = [
  AtomicU32::new(0),
  AtomicU32::new(0),
  AtomicU32::new(0),
  AtomicU32::new(0),
];
static LANE_RUNS: Mutex<VecDeque<LaneRun>> = Mutex::new(VecDeque::new());
static BACKGROUND_COUNT: AtomicU64 = AtomicU64::new(0);
static IN_BACKGROUND: AtomicBool = AtomicBool::new(false);
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
  /// Only on a "lane blocked" report: the job that held the thread while this one waited, empty
  /// for an ordinary job and a job that ran slow itself (issue #1759). `unknown` when none is known.
  #[uniffi(default = "")]
  pub blocked_by: String,
  /// How long the blocking job ran, in milliseconds (0 without a blocker).
  #[uniffi(default = 0.0)]
  pub blocker_run_ms: f64,
  /// Jobs queued behind the blocker when it ended, or behind the slow job when it did (0 if none).
  #[uniffi(default = 0)]
  pub waiters: u32,
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

/// Hears of a job that ran longer than the slow threshold, or of a job that waited that long
/// (`blocked_by` names what held its thread); at most one per job name a minute and a few per
/// process. Called on the core's thread: hand it on, don't block.
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
  lane: Option<Lane>,
  enqueued_at_ms: f64,
  enqueued: Instant,
  depth: u32,
  lifecycle: Lifecycle,
}

/// A job that started.
pub(crate) struct Running {
  queued: Queued,
  started_at_ms: f64,
  started: Instant,
  maybe_cpu_at_start: Option<Duration>,
  lifecycle: Lifecycle,
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

/// The queue jobs recorded here that overlap `start_ms` to `end_ms`, all of them: tests run in
/// parallel and share the records, so another test's longer jobs may push a job out of
/// [`perf_job_spans`]' longest few.
#[cfg(test)]
pub(crate) fn recorded_between(start_ms: f64, end_ms: f64) -> Vec<PerfJobSpan> {
  RECORDS
    .lock()
    .unwrap_or_else(PoisonError::into_inner)
    .iter()
    .filter(|span| span.done_at_ms >= start_ms && span.enqueued_at_ms <= end_ms)
    .cloned()
    .collect()
}

/// Where slow jobs and kept traces are reported; replaces the one before.
#[uniffi::export]
pub fn set_perf_listener(listener: Arc<dyn PerfListener>) {
  *LISTENER.lock().unwrap_or_else(PoisonError::into_inner) = Some(listener);
  trace::set_kept_hook(on_traces_kept);
  // Only a process that reports needs to tell a stall of its own from a held lane.
  stall_watch::start();
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
  Engine,
}

impl Lane {
  fn index(self) -> usize {
    match self {
      Self::Main => 0,
      Self::Query => 1,
      Self::Act => 2,
      Self::Engine => 3,
    }
  }

  fn depth(self) -> &'static AtomicU32 {
    &LANE_DEPTH[self.index()]
  }
}

/// A job that held a lane's thread, remembered to name the cause of what waited behind it.
#[derive(Clone, Debug)]
struct LaneRun {
  lane: Lane,
  name: &'static str,
  started_at_ms: f64,
  done_at_ms: f64,
  /// Jobs of the lane still queued when it ended.
  waiters: u32,
  /// The process was frozen while it ran: it blocked nothing (issue #2146).
  is_frozen: bool,
}

impl LaneRun {
  fn run_ms(&self) -> f64 {
    self.done_at_ms - self.started_at_ms
  }
}

/// What made a job slow.
#[derive(Clone, Debug, PartialEq)]
enum SlowKind {
  /// It ran long: its own report.
  Ran,
  /// It only waited: reported once as the lane being blocked, by the blocker.
  Waited,
}

/// A job is slow when it ran for the threshold; one that only waited is blocked, not slow. A
/// threshold of 0 turns it off, and so does a period the process was frozen for (`Frozen`).
fn slow_kind(threshold_ms: u64, waited: Duration, ran: Duration, frozen: Frozen) -> Option<SlowKind> {
  if threshold_ms == 0 {
    return None;
  }
  let threshold = threshold_ms as f64;
  if duration_ms(ran) >= threshold {
    (!frozen.is_run_frozen).then_some(SlowKind::Ran)
  } else if duration_ms(waited) >= threshold {
    (!frozen.is_wait_frozen).then_some(SlowKind::Waited)
  } else {
    None
  }
}

/// Which of a job's periods the process was frozen for (issue #2146).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Frozen {
  is_wait_frozen: bool,
  is_run_frozen: bool,
}

/// What the app told about its lifecycle at one moment: how often it went to the background, and
/// whether it is there now. A period overlapped the background when it started there or the count
/// changed during it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Lifecycle {
  backgrounds: u64,
  is_in_background: bool,
}

impl Lifecycle {
  fn now() -> Self {
    Self {
      backgrounds: BACKGROUND_COUNT.load(Ordering::Relaxed),
      is_in_background: IN_BACKGROUND.load(Ordering::Relaxed),
    }
  }

  /// Whether the app was in the background at some time between `self` and `later`.
  fn overlaps_background(self, later: Self) -> bool {
    self.is_in_background || later.backgrounds != self.backgrounds
  }
}

/// The app went to the background (`report_activity`): jobs running now may be frozen by the OS.
pub(crate) fn app_backgrounded() {
  BACKGROUND_COUNT.fetch_add(1, Ordering::Relaxed);
  IN_BACKGROUND.store(true, Ordering::Relaxed);
  stall_watch::backgrounded();
}

/// The app came to the foreground again.
pub(crate) fn app_foregrounded() {
  IN_BACKGROUND.store(false, Ordering::Relaxed);
  stall_watch::foregrounded();
}

/// Whether the process did not run for `period`: it was suspended (in the background, or the device
/// slept: `clock_gap`, wall time minus monotonic time) and its thread used almost no CPU time. A
/// platform that tells no CPU time (iOS, macOS, Windows) can only go by the suspension.
fn was_frozen(
  period: Duration,
  maybe_cpu: Option<Duration>,
  overlapped_background: bool,
  clock_gap: Duration,
) -> bool {
  let is_suspended = overlapped_background || clock_gap >= MIN_CLOCK_GAP;
  is_suspended && maybe_cpu.is_none_or(|cpu| cpu.as_secs_f64() < period.as_secs_f64() * FROZEN_CPU_SHARE)
}

/// How far the wall clock ran ahead of the monotonic one over a period of `period` that started at
/// `started_at_ms` and ended at `ended_at_ms`.
fn clock_gap(started_at_ms: f64, ended_at_ms: f64, period: Duration) -> Duration {
  Duration::try_from_secs_f64(((ended_at_ms - started_at_ms) - duration_ms(period)).max(0.0) / 1_000.0)
    .unwrap_or_default()
}

/// The job that held `lane` the longest while a job queued at `enqueued_ms` waited for `started_ms`.
fn blocker_of(runs: &VecDeque<LaneRun>, lane: Lane, enqueued_ms: f64, started_ms: f64) -> Option<&LaneRun> {
  runs
    .iter()
    .filter(|run| run.lane == lane && run.done_at_ms > enqueued_ms && run.started_at_ms < started_ms)
    .max_by(|a, b| a.run_ms().total_cmp(&b.run_ms()))
}

fn queued_on(kind: PerfJobKind, lane: Lane, type_name: &'static str) -> Queued {
  let depth = depth_of(kind).map_or(1, |depth| depth.fetch_add(1, Ordering::Relaxed) + 1);
  let maybe_lane = match kind {
    PerfJobKind::ReadQueue => Some(lane),
    PerfJobKind::EngineQueue => Some(Lane::Engine),
    _ => None,
  };
  if let Some(lane) = maybe_lane {
    lane.depth().fetch_add(1, Ordering::Relaxed);
  }
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
    lane: maybe_lane,
    enqueued_at_ms: now_ms(),
    enqueued: Instant::now(),
    depth,
    lifecycle: Lifecycle::now(),
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
    if let Some(lane) = self.lane {
      let _ = lane
        .depth()
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |depth| {
          Some(depth.saturating_sub(1))
        });
    }
    Running {
      started_at_ms: now_ms(),
      started: Instant::now(),
      maybe_cpu_at_start: logging::thread_cpu_time(),
      lifecycle: Lifecycle::now(),
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
    let done_at_ms = self.started_at_ms + duration_ms(ran);
    let maybe_cpu = self
      .maybe_cpu_at_start
      .zip(logging::thread_cpu_time())
      .map(|(before, after)| after.saturating_sub(before));
    let ended = Lifecycle::now();
    let frozen = Frozen {
      is_wait_frozen: was_frozen(
        waited,
        None,
        self.queued.lifecycle.overlaps_background(self.lifecycle),
        clock_gap(self.queued.enqueued_at_ms, self.started_at_ms, waited),
      ),
      is_run_frozen: was_frozen(
        ran,
        maybe_cpu,
        self.lifecycle.overlaps_background(ended),
        clock_gap(self.started_at_ms, now_ms(), ran),
      ),
    };
    // Remembered before the next job of the lane starts, so what waited behind this one finds it.
    if let Some(lane) = self.queued.lane.filter(|_| ran >= MIN_BLOCKER) {
      remember_lane_run(LaneRun {
        lane,
        name: self.queued.name,
        started_at_ms: self.started_at_ms,
        done_at_ms,
        waiters: lane.depth().load(Ordering::Relaxed),
        is_frozen: frozen.is_run_frozen,
      });
    }
    if waited + ran < MIN_KEPT && self.queued.depth < KEPT_DEPTH {
      return;
    }
    let span = PerfJobSpan {
      kind: self.queued.kind,
      op: String::new(),
      name: self.queued.name.to_owned(),
      enqueued_at_ms: self.queued.enqueued_at_ms,
      started_at_ms: self.started_at_ms,
      done_at_ms,
      queue_depth: self.queued.depth,
      maybe_cpu_ms: maybe_cpu.map(duration_ms),
      blocked_by: String::new(),
      blocker_run_ms: 0.0,
      waiters: 0,
    };
    keep(span.clone());
    match slow_kind(SLOW_MS.load(Ordering::Relaxed), waited, ran, frozen) {
      Some(SlowKind::Ran) => {
        let waiters = self
          .queued
          .lane
          .map_or(0, |lane| lane.depth().load(Ordering::Relaxed));
        let span = PerfJobSpan { waiters, ..span };
        match stall_in(self.started_at_ms, done_at_ms) {
          Some(stalled_ms) => report_stalled(span, stalled_ms),
          None => report_slow(self.queued.name, span),
        }
      },
      Some(SlowKind::Waited) => match stall_in(self.queued.enqueued_at_ms, self.started_at_ms) {
        Some(stalled_ms) => report_stalled(span, stalled_ms),
        None => report_blocked(self.queued.lane, span),
      },
      None => {},
    }
  }
}

/// Name of the engine lane's gateway event handling, as a lane-blocked report names it.
const GATEWAY_EVENT_JOB: &str = "gateway.event";

/// A gateway event applied on the engine thread between `started_at_ms` and `done_at_ms` (Unix
/// milliseconds): remembered as a job of the engine lane, so a job that waited behind it is
/// reported as blocked by `gateway.event` instead of `unknown` (issue #1900).
pub(crate) fn gateway_event_applied(started_at_ms: f64, done_at_ms: f64) {
  engine_work_done(GATEWAY_EVENT_JOB, started_at_ms, done_at_ms);
}

/// Work on the engine thread that is no job of its queue (the client's opening, a step of the
/// sync loop) ran between `started_at_ms` and `done_at_ms`: remembered as the lane's, so a job that
/// waited behind it names it instead of `unknown` (issue #2225).
pub(crate) fn engine_work_done(name: &'static str, started_at_ms: f64, done_at_ms: f64) {
  if done_at_ms - started_at_ms < duration_ms(MIN_BLOCKER) {
    return;
  }
  remember_lane_run(LaneRun {
    lane: Lane::Engine,
    name,
    started_at_ms,
    done_at_ms,
    waiters: Lane::Engine.depth().load(Ordering::Relaxed),
    is_frozen: false,
  });
}

fn remember_lane_run(run: LaneRun) {
  let mut runs = LANE_RUNS.lock().unwrap_or_else(PoisonError::into_inner);
  if runs.len() >= MAX_LANE_RUNS {
    runs.pop_front();
  }
  runs.push_back(run);
}

/// A job that only waited: one "lane blocked" report for the job that held the thread, not one
/// for every job behind it (issue #1759).
fn report_blocked(lane: Option<Lane>, span: PerfJobSpan) {
  let blocker = lane.and_then(|lane| {
    let runs = LANE_RUNS.lock().unwrap_or_else(PoisonError::into_inner);
    blocker_of(&runs, lane, span.enqueued_at_ms, span.started_at_ms).cloned()
  });
  if let Some((name, span)) = blocked_report(blocker, span) {
    report_slow(&name, span);
  }
}

/// The milliseconds of `started_at_ms` to `ended_at_ms` the whole process didn't run
/// ([`stall_watch`]), when that was most of it: then no job of the core held the lane, the OS did
/// (issue #2225).
fn stall_in(started_at_ms: f64, ended_at_ms: f64) -> Option<f64> {
  let length_ms = ended_at_ms - started_at_ms;
  if length_ms <= 0.0 {
    return None;
  }
  let stalled_ms = stall_watch::stalled_ms(started_at_ms, ended_at_ms);
  (stalled_ms >= length_ms * STALLED_SHARE).then_some(stalled_ms)
}

/// A job that waited or ran long because the process didn't run: one report for the stall, as the
/// blocker of the job's lane ("Read lane blocked: process_stall"), not one blaming its own job or
/// the job ahead of it.
fn report_stalled(span: PerfJobSpan, stalled_ms: f64) {
  report_slow(
    &format!("blocked:{PROCESS_STALL}"),
    PerfJobSpan {
      blocked_by: PROCESS_STALL.to_owned(),
      blocker_run_ms: stalled_ms,
      waiters: span.waiters.max(1),
      ..span
    },
  );
}

/// The report name and span of a job that waited behind `maybe_blocker`; none when the blocker did
/// not run because the process was frozen, as nothing blocked the lane then (issue #2146).
fn blocked_report(maybe_blocker: Option<LaneRun>, span: PerfJobSpan) -> Option<(String, PerfJobSpan)> {
  if maybe_blocker.as_ref().is_some_and(|run| run.is_frozen) {
    return None;
  }
  let (name, run_ms, waiters) = maybe_blocker.map_or(("unknown", 0.0, 1), |run| {
    (run.name, run.run_ms(), run.waiters.max(1))
  });
  Some((
    format!("blocked:{name}"),
    PerfJobSpan {
      blocked_by: name.to_owned(),
      blocker_run_ms: run_ms,
      waiters,
      ..span
    },
  ))
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
    blocked_by: String::new(),
    blocker_run_ms: 0.0,
    waiters: 0,
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
    blocked_by: String::new(),
    blocker_run_ms: 0.0,
    waiters: 0,
  };
  keep(span.clone());
  // Only the end of the query is known: frozen when the app is in the background now.
  let is_frozen = was_frozen(
    ran,
    None,
    Lifecycle::now().is_in_background,
    clock_gap(started_at_ms, now_ms(), ran),
  );
  let frozen = Frozen {
    is_wait_frozen: false,
    is_run_frozen: is_frozen,
  };
  if slow_kind(SLOW_MS.load(Ordering::Relaxed), Duration::ZERO, ran, frozen) == Some(SlowKind::Ran) {
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
  fn only_a_long_run_is_slow_a_long_wait_is_blocked() {
    let ms = Duration::from_millis;
    let live = Frozen::default();
    assert_eq!(slow_kind(1_000, ms(0), ms(2_000), live), Some(SlowKind::Ran));
    // The job that waited 3.7 s and ran 0 ms is not slow itself.
    assert_eq!(slow_kind(1_000, ms(3_718), ms(0), live), Some(SlowKind::Waited));
    // Both: its own run is the news.
    assert_eq!(slow_kind(1_000, ms(3_000), ms(1_000), live), Some(SlowKind::Ran));
    assert_eq!(slow_kind(1_000, ms(999), ms(999), live), None);
    assert_eq!(slow_kind(0, ms(9_000), ms(9_000), live), None);
  }

  mod frozen {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_job_the_background_froze_is_not_slow() {
      // Android 1.0.37, #2146: ran 298101 ms, cpu 2 ms, the app in the background.
      assert!(was_frozen(
        Duration::from_millis(298_101),
        Some(Duration::from_millis(2)),
        true,
        Duration::ZERO
      ));
      let frozen = Frozen {
        is_wait_frozen: false,
        is_run_frozen: true,
      };
      assert_eq!(
        slow_kind(1_000, Duration::ZERO, Duration::from_millis(298_101), frozen),
        None
      );
    }

    #[test]
    fn a_busy_job_in_the_background_is_still_slow() {
      assert!(!was_frozen(
        Duration::from_secs(5),
        Some(Duration::from_secs(4)),
        true,
        Duration::ZERO
      ));
    }

    #[test]
    fn a_lock_wait_in_the_foreground_is_still_reported() {
      // Low CPU time alone is no freeze: the thread waited for a lock.
      assert!(!was_frozen(
        Duration::from_secs(5),
        Some(Duration::from_millis(3)),
        false,
        Duration::ZERO
      ));
      assert_eq!(
        slow_kind(1_000, Duration::ZERO, Duration::from_secs(5), Frozen::default()),
        Some(SlowKind::Ran)
      );
    }

    #[test]
    fn a_device_sleep_is_a_freeze_without_the_lifecycle() {
      // The wall clock ran 4 s ahead of the monotonic one.
      assert!(was_frozen(
        Duration::from_secs(5),
        Some(Duration::from_millis(1)),
        false,
        4 * SECOND
      ));
      assert!(!was_frozen(
        Duration::from_secs(5),
        Some(Duration::from_millis(1)),
        false,
        Duration::from_millis(40)
      ));
    }

    #[test]
    fn without_cpu_time_the_suspension_decides() {
      // iOS, macOS and Windows tell no thread CPU time.
      assert!(was_frozen(Duration::from_secs(60), None, true, Duration::ZERO));
      assert!(!was_frozen(Duration::from_secs(60), None, false, Duration::ZERO));
    }

    #[test]
    fn a_wait_during_a_freeze_is_not_blocked() {
      let frozen = Frozen {
        is_wait_frozen: true,
        is_run_frozen: false,
      };
      assert_eq!(
        slow_kind(1_000, Duration::from_secs(300), Duration::ZERO, frozen),
        None
      );
    }

    #[test]
    fn the_background_overlaps_a_period_it_started_in_or_entered() {
      let before = Lifecycle {
        backgrounds: 3,
        is_in_background: false,
      };
      assert!(!before.overlaps_background(before));
      // Went to the background and came back meanwhile.
      let later = Lifecycle {
        backgrounds: 4,
        is_in_background: false,
      };
      assert!(before.overlaps_background(later));
      let in_background = Lifecycle {
        backgrounds: 3,
        is_in_background: true,
      };
      assert!(in_background.overlaps_background(in_background));
    }

    #[test]
    fn a_frozen_blocker_blocks_nothing() {
      let waiter = span(0.0, 4_000.0);
      let mut blocker = lane_run(Lane::Engine, "frozen_blocker", 0.0, 4_000.0);
      assert!(blocked_report(Some(blocker.clone()), waiter.clone()).is_some());
      blocker.is_frozen = true;
      assert!(blocked_report(Some(blocker), waiter.clone()).is_none());
      // An unknown blocker is still reported.
      assert!(blocked_report(None, waiter).is_some());
    }

    #[test]
    fn the_clock_gap_is_wall_time_beyond_the_run() {
      assert_eq!(clock_gap(1_000.0, 6_000.0, Duration::from_secs(1)), 4 * SECOND);
      // A wall clock set back is no gap.
      assert_eq!(
        clock_gap(6_000.0, 1_000.0, Duration::from_secs(1)),
        Duration::ZERO
      );
    }
  }

  fn lane_run(lane: Lane, name: &'static str, started: f64, done: f64) -> LaneRun {
    LaneRun {
      lane,
      name,
      started_at_ms: started,
      done_at_ms: done,
      waiters: 3,
      is_frozen: false,
    }
  }

  #[test]
  fn the_blocker_is_the_longest_job_of_the_lane_during_the_wait() {
    let runs = VecDeque::from([
      lane_run(Lane::Main, "settings_mark", 0.0, 50.0),
      // Started before the waiter queued and held the lane into its wait.
      lane_run(Lane::Main, "conversations", 100.0, 4_100.0),
      lane_run(Lane::Main, "snapshot", 4_100.0, 4_300.0),
      lane_run(Lane::Query, "messages", 100.0, 9_000.0),
      lane_run(Lane::Main, "link_requests", 4_300.0, 4_400.0),
    ]);
    let blocker = blocker_of(&runs, Lane::Main, 500.0, 4_300.0).expect("a blocker");
    assert_eq!(blocker.name, "conversations");
    assert_eq!(blocker.run_ms(), 4_000.0);
    // Another lane's jobs and those before the wait never block.
    assert!(blocker_of(&runs, Lane::Act, 500.0, 4_300.0).is_none());
    assert!(blocker_of(&runs, Lane::Main, 60.0, 90.0).is_none());
  }

  #[test]
  fn work_of_the_engine_thread_outside_its_jobs_is_named_as_the_blocker() {
    // Issue #2225: the client's opening and the sync loop's steps ran between the engine's jobs,
    // and what waited behind them was blocked by `unknown`. Far in the future, apart from the
    // other tests' windows.
    let base = 8.0e15;
    engine_work_done("open_client", base, base + 1_900.0);
    let runs = LANE_RUNS.lock().unwrap_or_else(PoisonError::into_inner);
    let blocker = blocker_of(&runs, Lane::Engine, base + 10.0, base + 1_900.0).expect("a blocker");
    assert_eq!(blocker.name, "open_client");
    assert!(
      blocker_of(&runs, Lane::Main, base + 10.0, base + 1_900.0).is_none(),
      "the engine's only"
    );
  }

  #[test]
  fn gateway_event_handling_is_named_as_the_blocker_of_the_engine_lane() {
    // Far in the future, so no other test's runs overlap this window.
    let base = 9.0e15;
    gateway_event_applied(base, base + 80.0);
    gateway_event_applied(base + 80.0, base + 160.0);
    let runs = LANE_RUNS.lock().unwrap_or_else(PoisonError::into_inner);
    let blocker = blocker_of(&runs, Lane::Engine, base + 10.0, base + 170.0).expect("a blocker");
    assert_eq!(blocker.name, "gateway.event");
    assert_eq!(blocker.lane, Lane::Engine);
    // Events too short to matter leave no record.
    drop(runs);
    gateway_event_applied(base + 500.0, base + 500.5);
    let runs = LANE_RUNS.lock().unwrap_or_else(PoisonError::into_inner);
    assert!(blocker_of(&runs, Lane::Engine, base + 400.0, base + 600.0).is_none());
  }

  #[test]
  fn a_job_the_process_did_not_run_for_is_blocked_by_the_stall_not_by_a_job() {
    // Issue #2225: every thread stood still for 7.2 s; the report named whichever job ran longest.
    // Far in the future, apart from the other tests' windows.
    let base = 7.0e15;
    crate::stall_watch::keep_stall(base + 500.0, base + 7_700.0);
    // The act ran from 43.7 s to 51.0 s of that window: almost all of it inside the stall.
    assert!(stall_in(base + 200.0, base + 7_480.0).is_some_and(|ms| (ms - 6_980.0).abs() < 1.0));
    // A job that ran next to the stall, not inside it, stays its own.
    assert!(stall_in(base + 7_700.0, base + 9_700.0).is_none());
    assert!(
      stall_in(base + 6_000.0, base + 10_000.0).is_none(),
      "under half of it"
    );

    let reports = Arc::new(Mutex::new(Vec::new()));
    struct Collect(Arc<Mutex<Vec<PerfJobSpan>>>);
    impl PerfListener for Collect {
      fn on_slow_job(&self, job: PerfJobSpan) {
        self.0.lock().unwrap().push(job);
      }
      fn on_traces_kept(&self) {}
    }
    *LISTENER.lock().unwrap() = Some(Arc::new(Collect(reports.clone())));
    report_stalled(span(base + 200.0, 7_280.0), 6_980.0);
    *LISTENER.lock().unwrap() = None;
    let reports = reports.lock().unwrap();
    // Another test may have used up the process's reports; one that got through names the stall.
    if let Some(report) = reports.iter().find(|r| r.blocked_by == PROCESS_STALL) {
      assert_eq!(report.blocker_run_ms, 6_980.0);
      assert_eq!(report.waiters, 1);
    }
  }

  #[test]
  fn a_stall_is_reported_once_for_the_blocker() {
    // Unique names: tests share the globals.
    remember_lane_run(lane_run(
      Lane::Engine,
      "stall_1759_blocker",
      1.0e12,
      1.0e12 + 4_000.0,
    ));
    let waiter = |name: &str| PerfJobSpan {
      kind: PerfJobKind::EngineQueue,
      op: String::new(),
      name: name.to_owned(),
      enqueued_at_ms: 1.0e12 + 100.0,
      started_at_ms: 1.0e12 + 4_000.0,
      done_at_ms: 1.0e12 + 4_001.0,
      queue_depth: 2,
      maybe_cpu_ms: None,
      blocked_by: String::new(),
      blocker_run_ms: 0.0,
      waiters: 0,
    };
    let reports = Arc::new(Mutex::new(Vec::new()));
    struct Collect(Arc<Mutex<Vec<PerfJobSpan>>>);
    impl PerfListener for Collect {
      fn on_slow_job(&self, job: PerfJobSpan) {
        self.0.lock().unwrap().push(job);
      }
      fn on_traces_kept(&self) {}
    }
    *LISTENER.lock().unwrap() = Some(Arc::new(Collect(reports.clone())));
    report_blocked(Some(Lane::Engine), waiter("a_waiter"));
    report_blocked(Some(Lane::Engine), waiter("another_waiter"));
    *LISTENER.lock().unwrap() = None;
    let reports: Vec<PerfJobSpan> = reports
      .lock()
      .unwrap()
      .iter()
      .filter(|r| r.blocked_by == "stall_1759_blocker")
      .cloned()
      .collect();
    // Another test may have used up the process's reports; when one got through, it is the only one.
    assert!(reports.len() <= 1, "one report per blocker, not per waiter");
    if let Some(report) = reports.first() {
      assert_eq!(report.blocker_run_ms, 3_999.0 + 1.0);
      assert_eq!(report.waiters, 3);
    }
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
      blocked_by: String::new(),
      blocker_run_ms: 0.0,
      waiters: 0,
    }
  }
}
