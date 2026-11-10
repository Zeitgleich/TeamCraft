//! Tracing (issue #1640, docs/tracing.md): one `tracing` layer that turns the spans written at the
//! chokepoints (HTTP client, store, queues, gateway, push, FFI, server requests) into timed records.
//!
//! Each span reports when it started, how long it was **queued** (made → first entered), **busy**
//! (entered) and **idle** (waiting after it first ran), and the CPU time of its thread while
//! entered where the platform tells it. The layer keeps the last closed spans in a ring (the
//! measures' steps, crash breadcrumbs), tracks open spans (where a hang is), and decides at the end
//! of a root span whether it becomes a transaction of its own: when it failed or was slow, else
//! sampled. The shells take kept trees with [`take_kept`] and send them with their flushes.
//!
//! Privacy: the layer is the scrubber. Field values pass [`trace_fields`](super::trace_fields): only
//! code names, route templates, HTTP methods, measure-like numbers and flags are kept, interned into
//! a bounded set of `&'static str`. Nothing else a caller writes reaches a record.

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::filter::Filtered;
use tracing_subscriber::layer::{Context, Filter, Layer};
use tracing_subscriber::registry::LookupSpan;

use super::trace_fields::{self, Kept, OTHER};
use super::{PerfStatus, SampleRate, SpanId, TraceId};

/// Closed spans the ring keeps; the oldest drop out.
const RING_CAPACITY: usize = 512;
/// Spans shorter than this (and not failed) don't go into the ring: they explain nothing.
const MIN_RING_MS: f64 = 2.0;
/// Open spans tracked at most; spans beyond are timed but not listed as open.
const MAX_OPEN: usize = 256;
/// Descendants a root collects while it runs; the longest [`MAX_CHILDREN`] are sent.
const MAX_COLLECTED: usize = 128;
pub const MAX_CHILDREN: usize = 31;
/// Spans that ran on the same queue while a root waited, at most.
pub const MAX_BLOCKED_BY: usize = 8;
/// A root queued shorter than this is not looked at for what blocked it.
const MIN_BLOCKED_MS: f64 = 50.0;
/// Numbers per span at most.
const MAX_DATA: usize = 6;
/// Kept trees waiting for the shell; the oldest drop out.
const MAX_KEPT: usize = 20;
/// Slow or failed roots kept per process start at most, and one per description in this time.
const MAX_SLOW_PER_PROCESS: u32 = 30;
const SLOW_GAP_MS: f64 = 60_000.0;
/// Sampled (neither slow nor failed) roots: this share of the trace rate (roots are many: every
/// queue job and request), at most so many a minute, and not shorter than [`MIN_SAMPLED_MS`].
const ROOT_SHARE_OF_TRACE_RATE: f64 = 0.01;
const MAX_SAMPLED_PER_MINUTE: u32 = 2;
const MIN_SAMPLED_MS: f64 = 5.0;
/// Closed spans a crash report carries as breadcrumbs, the newest.
pub const BREADCRUMB_SPANS: usize = 30;
/// Steps handed to one measure at most: the longest.
pub const MAX_PER_MEASURE: usize = 24;
/// Until the instance's setting arrives.
const DEFAULT_SLOW_MS: u64 = 1_000;

static IS_ON: AtomicBool = AtomicBool::new(false);
static SAMPLE_PER_MILLION: AtomicU32 = AtomicU32::new(0);
static SLOW_MS: AtomicU64 = AtomicU64::new(DEFAULT_SLOW_MS);
static CPU_CLOCK: OnceLock<CpuClock> = OnceLock::new();
static KEPT_HOOK: OnceLock<fn()> = OnceLock::new();
static STATE: Mutex<Shared> = Mutex::new(Shared::new());

/// The CPU time the calling thread used so far, where the platform tells it.
pub type CpuClock = fn() -> Option<Duration>;

/// The layer; one per process, installed with [`layer`].
#[derive(Clone, Copy, Debug, Default)]
pub struct TraceLayer;

/// Which callsites the layer listens to, and whether it listens now.
#[derive(Clone, Copy, Debug, Default)]
pub struct TraceFilter;

/// What the instance and the user set: collection follows crash reports being on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraceSettings {
  pub is_on: bool,
  /// Share of roots that are neither slow nor failed that are kept.
  pub trace_rate: SampleRate,
  /// A root at least this long is kept; 0: never for being slow.
  pub slow_ms: u32,
}

/// A span that ended.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedSpan {
  /// The span's name in the code: the protocol's operation (`http.client`, `queue.read`).
  pub op: &'static str,
  /// What it did: a route template or a name from the code.
  pub description: &'static str,
  pub trace_id: TraceId,
  pub span_id: SpanId,
  pub maybe_parent: Option<SpanId>,
  /// Unix time in milliseconds.
  pub start_ms: f64,
  pub end_ms: f64,
  /// Queued, busy and idle; `None` for a span that was never entered (a named step).
  pub maybe_phases: Option<Phases>,
  /// CPU time of the thread while the span was entered.
  pub maybe_cpu_ms: Option<f64>,
  pub status: PerfStatus,
  /// The error's class from the code (`timeout`), when it failed.
  pub maybe_error: Option<&'static str>,
  /// Numbers from the code (`status`, `rows`, `ttfb_ms`).
  pub data: Vec<(&'static str, f64)>,
}

/// Where a span's time went, in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Phases {
  /// Made → first entered: waiting in a queue or for its first poll.
  pub queued_ms: f64,
  /// Entered: working (or blocking its thread).
  pub busy_ms: f64,
  /// After it first ran, not entered: waiting on the network, a lock, a channel.
  pub idle_ms: f64,
}

/// A root span kept for sending, with what happened under it.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceTree {
  pub root: ClosedSpan,
  /// Its descendants, the longest, in time order.
  pub spans: Vec<ClosedSpan>,
  /// Other roots of the same queue that ran while it waited, the longest.
  pub blocked_by: Vec<ClosedSpan>,
  pub reason: KeepReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepReason {
  Failed,
  Slow,
  Sampled,
}

/// A closed span as the desktop shell hands it to its page (`perf_spans`, `perf_traces`): every
/// text is checked again when it arrives ([`WireSpan::into_span`]), so a shell can't widen what
/// the page sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireSpan {
  pub op: String,
  pub description: String,
  pub trace_id: String,
  pub span_id: String,
  pub maybe_parent: Option<String>,
  pub start_ms: f64,
  pub end_ms: f64,
  /// Queued, busy and idle milliseconds.
  pub maybe_phases: Option<[f64; 3]>,
  pub maybe_cpu_ms: Option<f64>,
  pub status: String,
  pub maybe_error: Option<String>,
  pub data: Vec<(String, f64)>,
}

/// A kept tree over the same boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireTraceTree {
  pub root: WireSpan,
  pub spans: Vec<WireSpan>,
  pub blocked_by: Vec<WireSpan>,
  /// `failed`, `slow` or `sampled`.
  pub reason: String,
}

/// A span still running.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenSpan {
  pub op: &'static str,
  pub description: &'static str,
  /// Unix time in milliseconds.
  pub start_ms: f64,
  /// Whether it has been entered at all (ran), or is still queued.
  pub has_started: bool,
}

/// A span's state while it is open (the registry's extension).
struct SpanState {
  op: &'static str,
  maybe_name: Option<&'static str>,
  maybe_method: Option<&'static str>,
  maybe_route: Option<&'static str>,
  maybe_error: Option<&'static str>,
  data: Vec<(&'static str, f64)>,
  trace_id: TraceId,
  span_id: SpanId,
  maybe_parent: Option<SpanId>,
  start_unix_ms: f64,
  start_mono_ms: f64,
  maybe_first_enter_mono_ms: Option<f64>,
  busy_ms: f64,
  /// Entries without their exit yet (a span may be entered again while entered).
  depth: u32,
  entered_at_mono_ms: f64,
  maybe_cpu_at_enter: Option<Duration>,
  maybe_cpu_ms: Option<f64>,
  /// Only a root's: its closed descendants.
  children: Vec<ClosedSpan>,
}

/// The process-wide part: ring, open spans, kept trees and the caps.
struct Shared {
  ring: VecDeque<ClosedSpan>,
  maybe_open: Option<HashMap<u64, OpenSpan>>,
  kept: VecDeque<TraceTree>,
  maybe_last_slow: Option<HashMap<&'static str, f64>>,
  slow_kept: u32,
  sample_window_start_ms: f64,
  sampled_in_window: u32,
}

/// The layer with its filter, to add to a registry.
pub fn layer<S>() -> Filtered<TraceLayer, TraceFilter, S>
where
  S: Subscriber + for<'a> LookupSpan<'a>,
{
  teamcraft_proto::set_trace_header_source(sentry_trace_header);
  TraceLayer.with_filter(TraceFilter)
}

/// Turns collection on or off and sets the sampling; takes effect for spans made from now on.
pub fn configure(settings: TraceSettings) {
  // Rounding: a millionth is finer than any rate an admin sets.
  let per_million = (settings.trace_rate.get() * ROOT_SHARE_OF_TRACE_RATE * 1_000_000.0).round() as u32;
  SAMPLE_PER_MILLION.store(per_million, Ordering::Relaxed);
  SLOW_MS.store(u64::from(settings.slow_ms), Ordering::Relaxed);
  IS_ON.store(settings.is_on, Ordering::Relaxed);
}

/// Whether spans are collected now.
pub fn is_on() -> bool {
  IS_ON.load(Ordering::Relaxed)
}

/// Measures each span's CPU time with `clock` (once per process; later calls change nothing).
pub fn set_cpu_clock(clock: CpuClock) {
  // A second install is ignored on purpose: one clock per process.
  let _ = CPU_CLOCK.set(clock);
}

/// Calls `hook` after a tree was kept (once per process; later calls change nothing): the mobile
/// shells take the trees then. Called on the thread that closed the span: hand it on, don't block.
pub fn set_kept_hook(hook: fn()) {
  // A second install is ignored on purpose: one hook per process.
  let _ = KEPT_HOOK.set(hook);
}

/// The trees kept since the last call, oldest first.
pub fn take_kept() -> Vec<TraceTree> {
  lock().kept.drain(..).collect()
}

/// The closed spans that overlap `start_ms` to `end_ms` (Unix milliseconds): the longest, at most
/// [`MAX_PER_MEASURE`], in time order. A measure takes its steps from here.
pub fn recent(start_ms: f64, end_ms: f64) -> Vec<ClosedSpan> {
  let shared = lock();
  let mut overlapping: Vec<ClosedSpan> = shared
    .ring
    .iter()
    .filter(|span| span.end_ms >= start_ms && span.start_ms <= end_ms)
    .cloned()
    .collect();
  drop(shared);
  keep_longest(&mut overlapping, MAX_PER_MEASURE);
  overlapping
}

/// The spans still open, the oldest first: where the process is stuck when it is.
pub fn open_spans() -> Vec<OpenSpan> {
  let shared = lock();
  let mut open: Vec<OpenSpan> = shared
    .maybe_open
    .as_ref()
    .map(|open| open.values().cloned().collect())
    .unwrap_or_default();
  drop(shared);
  open.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
  open
}

/// Lines for a crash report's breadcrumbs: the newest closed spans, then the open ones with their
/// age at `now_ms` (Unix milliseconds).
pub fn breadcrumb_lines(now_ms: f64) -> Vec<String> {
  let closed: Vec<ClosedSpan> = {
    let shared = lock();
    let skip = shared.ring.len().saturating_sub(BREADCRUMB_SPANS);
    shared.ring.iter().skip(skip).cloned().collect()
  };
  let mut lines: Vec<String> = closed.iter().map(ClosedSpan::breadcrumb).collect();
  lines.extend(open_spans().iter().map(|span| span.breadcrumb(now_ms)));
  lines
}

/// The `sentry-trace` header for a request made inside the current span: `<trace>-<span>`, no
/// sampling flag (each side decides at its own end). `None` outside a collected span.
pub fn sentry_trace_header() -> Option<String> {
  tracing::Span::current()
    .with_subscriber(|(id, dispatch)| {
      let registry = dispatch.downcast_ref::<tracing_subscriber::Registry>()?;
      let span = registry.span(id)?;
      let extensions = span.extensions();
      let state = extensions.get::<SpanState>()?;
      Some(format!("{}-{}", state.trace_id, state.span_id))
    })
    .flatten()
}

impl<S: Subscriber> Filter<S> for TraceFilter {
  fn enabled(&self, metadata: &Metadata<'_>, _context: &Context<'_, S>) -> bool {
    IS_ON.load(Ordering::Relaxed) && is_traced(metadata)
  }

  fn callsite_enabled(&self, metadata: &'static Metadata<'static>) -> Interest {
    if is_traced(metadata) {
      // Sometimes: collection turns on and off at run time.
      Interest::sometimes()
    } else {
      Interest::never()
    }
  }
}

impl<S> Layer<S> for TraceLayer
where
  S: Subscriber + for<'a> LookupSpan<'a>,
{
  fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
    let Some(span) = context.span(id) else {
      return;
    };
    let maybe_parent_ids = span.parent().and_then(|parent| {
      let extensions = parent.extensions();
      extensions
        .get::<SpanState>()
        .map(|state| (state.trace_id, state.span_id))
    });
    let mut state = SpanState::new(attributes.metadata().name(), maybe_parent_ids);
    attributes.record(&mut FieldVisitor(&mut state));
    note_open(id, &state);
    span.extensions_mut().insert(state);
  }

  fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
    if let Some(span) = context.span(id)
      && let Some(state) = span.extensions_mut().get_mut::<SpanState>()
    {
      values.record(&mut FieldVisitor(state));
    }
  }

  fn on_enter(&self, id: &Id, context: Context<'_, S>) {
    if let Some(span) = context.span(id)
      && let Some(state) = span.extensions_mut().get_mut::<SpanState>()
    {
      let is_first = state.maybe_first_enter_mono_ms.is_none();
      state.enter(mono_ms(), cpu_now());
      if is_first {
        note_started(id);
      }
    }
  }

  fn on_exit(&self, id: &Id, context: Context<'_, S>) {
    if let Some(span) = context.span(id)
      && let Some(state) = span.extensions_mut().get_mut::<SpanState>()
    {
      state.exit(mono_ms(), cpu_now());
    }
  }

  fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
    if event.metadata().target() != SQL_TARGET {
      return;
    }
    let Some(current) = context.event_span(event) else {
      return;
    };
    let mut statement = SqlStatement::default();
    event.record(&mut statement);
    let Some(root) = current.scope().from_root().next() else {
      return;
    };
    let (trace_id, parent) = {
      let extensions = current.extensions();
      let Some(state) = extensions.get::<SpanState>() else {
        return;
      };
      (state.trace_id, state.span_id)
    };
    let child = statement.into_span(trace_id, parent);
    push_ring(&child);
    if let Some(state) = root.extensions_mut().get_mut::<SpanState>() {
      state.collect(child);
    }
  }

  fn on_close(&self, id: Id, context: Context<'_, S>) {
    let Some(span) = context.span(&id) else {
      return;
    };
    let Some(mut state) = span.extensions_mut().remove::<SpanState>() else {
      return;
    };
    let children = std::mem::take(&mut state.children);
    let closed = state.close(mono_ms());
    note_closed(&id);
    push_ring(&closed);
    let maybe_root = span.scope().from_root().next().filter(|root| root.id() != id);
    match maybe_root {
      Some(root) => {
        if let Some(root_state) = root.extensions_mut().get_mut::<SpanState>() {
          root_state.collect(closed);
        }
      },
      None => keep_if_worth_it(closed, children),
    }
  }
}

/// sqlx's statement log (the server's SQL), turned into `db.sql` spans.
const SQL_TARGET: &str = "sqlx::query";

/// Spans of TeamCraft's crates named like an operation (`http.client`), and sqlx's statements.
fn is_traced(metadata: &Metadata<'_>) -> bool {
  if metadata.is_event() {
    return metadata.target() == SQL_TARGET;
  }
  metadata.is_span() && metadata.target().starts_with("teamcraft") && is_op(metadata.name())
}

fn is_op(name: &str) -> bool {
  name.len() <= 32
    && name.contains('.')
    && name
      .bytes()
      .all(|byte| byte.is_ascii_lowercase() || byte == b'.' || byte == b'_')
}

fn keep_if_worth_it(root: ClosedSpan, mut spans: Vec<ClosedSpan>) {
  let Some(reason) = keep_reason(&root) else {
    return;
  };
  keep_longest(&mut spans, MAX_CHILDREN);
  let blocked_by = blocked_by(&root);
  let tree = TraceTree {
    root,
    spans,
    blocked_by,
    reason,
  };
  {
    let mut shared = lock();
    if shared.kept.len() >= MAX_KEPT {
      shared.kept.pop_front();
    }
    shared.kept.push_back(tree);
  }
  if let Some(hook) = KEPT_HOOK.get() {
    hook();
  }
}

fn keep_reason(root: &ClosedSpan) -> Option<KeepReason> {
  let took = root.duration_ms();
  let slow_ms = SLOW_MS.load(Ordering::Relaxed) as f64;
  let maybe_reason = if root.status != PerfStatus::Ok {
    Some(KeepReason::Failed)
  } else if slow_ms > 0.0 && took >= slow_ms {
    Some(KeepReason::Slow)
  } else {
    None
  };
  let now = mono_ms();
  let mut shared = lock();
  match maybe_reason {
    Some(reason) => shared.allows_slow(root.description, now).then_some(reason),
    None => {
      let roll = random_u64() % 1_000_000;
      let is_sampled = took >= MIN_SAMPLED_MS && roll < u64::from(SAMPLE_PER_MILLION.load(Ordering::Relaxed));
      (is_sampled && shared.allows_sampled(now)).then_some(KeepReason::Sampled)
    },
  }
}

/// Other roots of the same operation (queue) that ran while `root` waited to start.
fn blocked_by(root: &ClosedSpan) -> Vec<ClosedSpan> {
  let Some(phases) = root.maybe_phases else {
    return Vec::new();
  };
  if phases.queued_ms < MIN_BLOCKED_MS {
    return Vec::new();
  }
  let wait_end_ms = root.start_ms + phases.queued_ms;
  let mut blocking: Vec<ClosedSpan> = lock()
    .ring
    .iter()
    .filter(|span| {
      span.maybe_parent.is_none()
        && span.op == root.op
        && span.span_id != root.span_id
        && span.end_ms > root.start_ms
        && span.start_ms < wait_end_ms
    })
    .cloned()
    .collect();
  keep_longest(&mut blocking, MAX_BLOCKED_BY);
  blocking
}

/// The longest `max` spans, then in time order.
fn keep_longest(spans: &mut Vec<ClosedSpan>, max: usize) {
  spans.sort_by(|a, b| b.duration_ms().total_cmp(&a.duration_ms()));
  spans.truncate(max);
  spans.sort_by(|a, b| a.start_ms.total_cmp(&b.start_ms));
}

fn push_ring(span: &ClosedSpan) {
  if span.duration_ms() < MIN_RING_MS && span.status == PerfStatus::Ok {
    return;
  }
  let mut shared = lock();
  if shared.ring.len() >= RING_CAPACITY {
    shared.ring.pop_front();
  }
  shared.ring.push_back(span.clone());
}

fn note_open(id: &Id, state: &SpanState) {
  let mut shared = lock();
  let open = shared.maybe_open.get_or_insert_with(HashMap::new);
  if open.len() < MAX_OPEN {
    open.insert(
      id.into_u64(),
      OpenSpan {
        op: state.op,
        description: state.description(),
        start_ms: state.start_unix_ms,
        has_started: false,
      },
    );
  }
}

fn note_started(id: &Id) {
  if let Some(open) = lock().maybe_open.as_mut()
    && let Some(span) = open.get_mut(&id.into_u64())
  {
    span.has_started = true;
  }
}

fn note_closed(id: &Id) {
  if let Some(open) = lock().maybe_open.as_mut() {
    open.remove(&id.into_u64());
  }
}

fn lock() -> std::sync::MutexGuard<'static, Shared> {
  STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn cpu_now() -> Option<Duration> {
  CPU_CLOCK.get().and_then(|clock| clock())
}

/// Random IDs for traces and spans: not secrets, only unique, so a hash of a counter and the
/// clock under a per-process random key is enough and needs no RNG crate.
fn random_u64() -> u64 {
  static KEYS: OnceLock<RandomState> = OnceLock::new();
  static COUNTER: AtomicU64 = AtomicU64::new(0);
  let mut hasher = KEYS.get_or_init(RandomState::new).build_hasher();
  hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
  hasher.write_u64(unix_ms().to_bits());
  hasher.finish() | 1
}

fn new_trace_id() -> TraceId {
  let high = u128::from(random_u64()) << 64;
  TraceId::from_u128(high | u128::from(random_u64()))
}

fn new_span_id() -> SpanId {
  SpanId::from_u64(random_u64())
}

/// A monotonic clock in milliseconds (the browser's has whole milliseconds).
fn mono_ms() -> f64 {
  #[cfg(not(target_arch = "wasm32"))]
  {
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed().as_secs_f64() * 1_000.0
  }
  #[cfg(target_arch = "wasm32")]
  {
    js_sys::Date::now()
  }
}

fn unix_ms() -> f64 {
  #[cfg(not(target_arch = "wasm32"))]
  {
    std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1_000.0)
  }
  #[cfg(target_arch = "wasm32")]
  {
    js_sys::Date::now()
  }
}

impl SpanState {
  fn new(op: &'static str, maybe_parent_ids: Option<(TraceId, SpanId)>) -> Self {
    let (trace_id, maybe_parent) = match maybe_parent_ids {
      Some((trace, parent)) => (trace, Some(parent)),
      None => (new_trace_id(), None),
    };
    Self {
      op,
      maybe_name: None,
      maybe_method: None,
      maybe_route: None,
      maybe_error: None,
      data: Vec::new(),
      trace_id,
      span_id: new_span_id(),
      maybe_parent,
      start_unix_ms: unix_ms(),
      start_mono_ms: mono_ms(),
      maybe_first_enter_mono_ms: None,
      busy_ms: 0.0,
      depth: 0,
      entered_at_mono_ms: 0.0,
      maybe_cpu_at_enter: None,
      maybe_cpu_ms: None,
      children: Vec::new(),
    }
  }

  fn keep(&mut self, kept: Kept) {
    match kept {
      Kept::Name(name) => self.maybe_name = Some(name),
      Kept::Method(method) => self.maybe_method = Some(method),
      Kept::Route(route) => self.maybe_route = Some(route),
      Kept::Error(class) => self.maybe_error = Some(class),
      Kept::Number(name, value) => self.set_data(name, value),
      Kept::Flag(name, value) => self.set_data(name, if value { 1.0 } else { 0.0 }),
      Kept::Dropped => {},
    }
  }

  fn set_data(&mut self, name: &'static str, value: f64) {
    if let Some(slot) = self.data.iter_mut().find(|(known, _)| *known == name) {
      slot.1 = value;
    } else if self.data.len() < MAX_DATA {
      self.data.push((name, value));
    }
  }

  /// A trace continued from a caller (`sentry-trace` of an incoming request): `<trace>-<span>`.
  fn continue_trace(&mut self, header: &str) {
    let mut parts = header.trim().split('-');
    let (Some(trace), Some(span)) = (parts.next(), parts.next()) else {
      return;
    };
    if self.maybe_parent.is_some() {
      return;
    }
    if let (Some(trace_id), Some(parent)) = (TraceId::parse(trace), SpanId::parse(span)) {
      self.trace_id = trace_id;
      self.maybe_parent = Some(parent);
    }
  }

  fn enter(&mut self, now_ms: f64, maybe_cpu: Option<Duration>) {
    if self.depth == 0 {
      self.entered_at_mono_ms = now_ms;
      self.maybe_cpu_at_enter = maybe_cpu;
      self.maybe_first_enter_mono_ms.get_or_insert(now_ms);
    }
    self.depth += 1;
  }

  fn exit(&mut self, now_ms: f64, maybe_cpu: Option<Duration>) {
    self.depth = self.depth.saturating_sub(1);
    if self.depth > 0 {
      return;
    }
    self.busy_ms += (now_ms - self.entered_at_mono_ms).max(0.0);
    if let Some((before, after)) = self.maybe_cpu_at_enter.take().zip(maybe_cpu) {
      let used = after.saturating_sub(before).as_secs_f64() * 1_000.0;
      *self.maybe_cpu_ms.get_or_insert(0.0) += used;
    }
  }

  fn collect(&mut self, child: ClosedSpan) {
    if self.children.len() < MAX_COLLECTED {
      self.children.push(child);
    } else if let Some(shortest) = self
      .children
      .iter_mut()
      .min_by(|a, b| a.duration_ms().total_cmp(&b.duration_ms()))
      && shortest.duration_ms() < child.duration_ms()
    {
      *shortest = child;
    }
  }

  fn description(&self) -> &'static str {
    match (self.maybe_method, self.maybe_route, self.maybe_name) {
      (Some(method), Some(route), _) => trace_fields::intern(&format!("{method} {route}")),
      (_, Some(route), _) => route,
      (_, None, Some(name)) => name,
      (_, None, None) => self.op,
    }
  }

  fn status(&self) -> PerfStatus {
    if let Some(class) = self.maybe_error {
      return match class {
        "timeout" | "timed_out" | "deadline" => PerfStatus::TimedOut,
        "cancelled" | "canceled" => PerfStatus::Cancelled,
        "unavailable" | "offline" | "connect" | "network" => PerfStatus::Unavailable,
        _ => PerfStatus::Error,
      };
    }
    let maybe_http = self
      .data
      .iter()
      .find(|(name, _)| *name == "status")
      .map(|(_, status)| *status);
    match maybe_http {
      Some(503.0) => PerfStatus::Unavailable,
      Some(status) if status >= 500.0 => PerfStatus::Error,
      _ => PerfStatus::Ok,
    }
  }

  fn close(self, now_mono_ms: f64) -> ClosedSpan {
    let total_ms = (now_mono_ms - self.start_mono_ms).max(0.0);
    let maybe_phases = self.maybe_first_enter_mono_ms.map(|first| {
      let queued_ms = (first - self.start_mono_ms).clamp(0.0, total_ms);
      let busy_ms = self.busy_ms.min(total_ms - queued_ms);
      Phases {
        queued_ms,
        busy_ms,
        idle_ms: (total_ms - queued_ms - busy_ms).max(0.0),
      }
    });
    ClosedSpan {
      op: self.op,
      description: self.description(),
      trace_id: self.trace_id,
      span_id: self.span_id,
      maybe_parent: self.maybe_parent,
      start_ms: self.start_unix_ms,
      end_ms: self.start_unix_ms + total_ms,
      maybe_phases,
      maybe_cpu_ms: self.maybe_cpu_ms,
      status: self.status(),
      maybe_error: self.maybe_error,
      data: self.data,
    }
  }
}

impl ClosedSpan {
  pub fn duration_ms(&self) -> f64 {
    self.end_ms - self.start_ms
  }

  fn breadcrumb(&self) -> String {
    let mut line = format!(
      "span {} {} {:.0}ms",
      self.op,
      self.description,
      self.duration_ms()
    );
    if let Some(phases) = self.maybe_phases {
      line.push_str(&format!(
        " (queued {:.0}, busy {:.0}, idle {:.0})",
        phases.queued_ms, phases.busy_ms, phases.idle_ms
      ));
    }
    if let Some(class) = self.maybe_error {
      line.push_str(" error=");
      line.push_str(class);
    }
    line
  }
}

impl From<&ClosedSpan> for WireSpan {
  fn from(span: &ClosedSpan) -> Self {
    Self {
      op: span.op.to_owned(),
      description: span.description.to_owned(),
      trace_id: span.trace_id.to_string(),
      span_id: span.span_id.to_string(),
      maybe_parent: span.maybe_parent.map(|parent| parent.to_string()),
      start_ms: span.start_ms,
      end_ms: span.end_ms,
      maybe_phases: span
        .maybe_phases
        .map(|phases| [phases.queued_ms, phases.busy_ms, phases.idle_ms]),
      maybe_cpu_ms: span.maybe_cpu_ms,
      status: span.status.as_str().to_owned(),
      maybe_error: span.maybe_error.map(str::to_owned),
      data: span
        .data
        .iter()
        .map(|(name, value)| ((*name).to_owned(), *value))
        .collect(),
    }
  }
}

impl WireSpan {
  /// Back to a span, with every text checked: an operation not shaped like one, a bad ID or a
  /// time that isn't one drops the span; a description or error that isn't a name becomes `other`.
  pub fn into_span(self) -> Option<ClosedSpan> {
    let op = trace_fields::code_name(&self.op);
    let is_time = |ms: f64| ms.is_finite() && ms >= 0.0;
    if !is_op(op) || !is_time(self.start_ms) || !is_time(self.end_ms) {
      return None;
    }
    let maybe_parent = match self.maybe_parent {
      Some(parent) => Some(SpanId::parse(&parent)?),
      None => None,
    };
    let finite = |ms: f64| ms.is_finite().then_some(ms.max(0.0));
    Some(ClosedSpan {
      op,
      description: trace_fields::checked_description(&self.description),
      trace_id: TraceId::parse(&self.trace_id)?,
      span_id: SpanId::parse(&self.span_id)?,
      maybe_parent,
      start_ms: self.start_ms,
      end_ms: self.end_ms.max(self.start_ms),
      maybe_phases: self.maybe_phases.and_then(|[queued, busy, idle]| {
        Some(Phases {
          queued_ms: finite(queued)?,
          busy_ms: finite(busy)?,
          idle_ms: finite(idle)?,
        })
      }),
      maybe_cpu_ms: self.maybe_cpu_ms.and_then(finite),
      status: PerfStatus::from_protocol(&self.status),
      maybe_error: self.maybe_error.as_deref().map(trace_fields::code_name),
      data: self
        .data
        .iter()
        .filter_map(|(name, value)| Some((trace_fields::checked_number_name(name)?, finite(*value)?)))
        .take(MAX_DATA)
        .collect(),
    })
  }
}

impl From<&TraceTree> for WireTraceTree {
  fn from(tree: &TraceTree) -> Self {
    Self {
      root: WireSpan::from(&tree.root),
      spans: tree.spans.iter().map(WireSpan::from).collect(),
      blocked_by: tree.blocked_by.iter().map(WireSpan::from).collect(),
      reason: tree.reason.as_str().to_owned(),
    }
  }
}

impl WireTraceTree {
  /// Back to a tree, every span checked ([`WireSpan::into_span`]), the bounds kept.
  pub fn into_tree(self) -> Option<TraceTree> {
    let reason = match self.reason.as_str() {
      "failed" => KeepReason::Failed,
      "slow" => KeepReason::Slow,
      "sampled" => KeepReason::Sampled,
      _ => return None,
    };
    let checked = |spans: Vec<WireSpan>, max: usize| -> Vec<ClosedSpan> {
      spans
        .into_iter()
        .filter_map(WireSpan::into_span)
        .take(max)
        .collect()
    };
    Some(TraceTree {
      root: self.root.into_span()?,
      spans: checked(self.spans, MAX_CHILDREN),
      blocked_by: checked(self.blocked_by, MAX_BLOCKED_BY),
      reason,
    })
  }
}

impl OpenSpan {
  fn breadcrumb(&self, now_ms: f64) -> String {
    let state = if self.has_started { "running" } else { "queued" };
    format!(
      "open {} {} {state} for {:.0}ms",
      self.op,
      self.description,
      (now_ms - self.start_ms).max(0.0)
    )
  }
}

impl Shared {
  const fn new() -> Self {
    Self {
      ring: VecDeque::new(),
      maybe_open: None,
      kept: VecDeque::new(),
      maybe_last_slow: None,
      slow_kept: 0,
      sample_window_start_ms: 0.0,
      sampled_in_window: 0,
    }
  }

  fn allows_slow(&mut self, description: &'static str, now_ms: f64) -> bool {
    if self.slow_kept >= MAX_SLOW_PER_PROCESS {
      return false;
    }
    let last = self.maybe_last_slow.get_or_insert_with(HashMap::new);
    if last.get(description).is_some_and(|at| now_ms - at < SLOW_GAP_MS) {
      return false;
    }
    last.insert(description, now_ms);
    self.slow_kept += 1;
    true
  }

  fn allows_sampled(&mut self, now_ms: f64) -> bool {
    if now_ms - self.sample_window_start_ms >= 60_000.0 {
      self.sample_window_start_ms = now_ms;
      self.sampled_in_window = 0;
    }
    if self.sampled_in_window >= MAX_SAMPLED_PER_MINUTE {
      return false;
    }
    self.sampled_in_window += 1;
    true
  }
}

/// Records a span's fields through the checks of [`trace_fields`].
struct FieldVisitor<'a>(&'a mut SpanState);

impl Visit for FieldVisitor<'_> {
  fn record_str(&mut self, field: &Field, value: &str) {
    if field.name() == "sentry_trace" {
      self.0.continue_trace(value);
    } else {
      self.0.keep(trace_fields::keep_text(field.name(), value));
    }
  }

  fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
    // Only fields that may hold a name are formatted at all; `%value` arrives here too.
    if is_text_field(field.name()) {
      self.record_str(field, &format!("{value:?}"));
    }
  }

  fn record_u64(&mut self, field: &Field, value: u64) {
    // Precision: measures are far below 2^53.
    self.0.keep(trace_fields::keep_number(field.name(), value as f64));
  }

  fn record_i64(&mut self, field: &Field, value: i64) {
    // Precision: measures are far below 2^53.
    self.0.keep(trace_fields::keep_number(field.name(), value as f64));
  }

  fn record_f64(&mut self, field: &Field, value: f64) {
    self.0.keep(trace_fields::keep_number(field.name(), value));
  }

  fn record_bool(&mut self, field: &Field, value: bool) {
    self.0.keep(trace_fields::keep_flag(field.name(), value));
  }

  fn record_error(&mut self, _field: &Field, _value: &(dyn std::error::Error + 'static)) {
    // The message may carry anything; only the fact that it failed is kept.
    self.0.keep(Kept::Error("error"));
  }
}

fn is_text_field(name: &str) -> bool {
  matches!(
    name,
    "name"
      | "kind"
      | "event"
      | "job"
      | "step"
      | "error"
      | "method"
      | "route"
      | "path"
      | "url"
      | "sentry_trace"
  )
}

/// One SQL statement as sqlx logs it.
#[derive(Default)]
struct SqlStatement {
  maybe_summary: Option<&'static str>,
  elapsed_ms: f64,
  maybe_rows: Option<f64>,
}

impl SqlStatement {
  fn into_span(self, trace_id: TraceId, parent: SpanId) -> ClosedSpan {
    let end_ms = unix_ms();
    let mut data = Vec::new();
    if let Some(rows) = self.maybe_rows {
      data.push(("rows", rows));
    }
    ClosedSpan {
      op: "db.sql",
      description: self.maybe_summary.unwrap_or(OTHER),
      trace_id,
      span_id: new_span_id(),
      maybe_parent: Some(parent),
      start_ms: end_ms - self.elapsed_ms,
      end_ms,
      maybe_phases: None,
      maybe_cpu_ms: None,
      status: PerfStatus::Ok,
      maybe_error: None,
      data,
    }
  }
}

impl Visit for SqlStatement {
  fn record_str(&mut self, field: &Field, value: &str) {
    if field.name() == "summary" {
      self.maybe_summary = Some(trace_fields::intern(&trace_fields::sql_summary(value)));
    }
  }

  fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
    if field.name() == "summary" {
      self.record_str(field, &format!("{value:?}"));
    }
  }

  fn record_f64(&mut self, field: &Field, value: f64) {
    if field.name() == "elapsed_secs" && value.is_finite() {
      self.elapsed_ms = (value * 1_000.0).max(0.0);
    }
  }

  fn record_u64(&mut self, field: &Field, value: u64) {
    if field.name() == "rows_returned" || field.name() == "rows_affected" {
      // Precision: row counts are far below 2^53.
      *self.maybe_rows.get_or_insert(0.0) += value as f64;
    }
  }
}

#[cfg(test)]
mod tests;
