//! Performance data (issue #1521): one measured operation as a Sentry `transaction`, sent to the
//! same backend as crash reports (docs/crash-reports.md, "Performance data").
//!
//! Privacy by construction: a transaction's name is a [`PerfOperation`], its spans' operation and
//! description, its tags and its measurement names are `&'static str` written in the code. There
//! is no field a caller could fill with a message, a name, a file name or an ID at run time; the
//! only run-time values are times, numbers and the random installation ID.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::Environment;
use super::event::{AppInfo, EventId, InstallationId, RELEASE_PREFIX, hex, seconds_f64};
use super::trace::{ClosedSpan, KeepReason, TraceTree};

/// Spans per transaction at most; later ones are left out.
pub const MAX_SPANS: usize = 32;
/// Tags and measurements per transaction at most.
pub const MAX_TAGS: usize = 8;
pub const MAX_MEASUREMENTS: usize = 8;

/// One measured operation, finished, ready to queue.
#[derive(Clone, Debug, PartialEq)]
pub struct Transaction {
  event_id: EventId,
  trace_id: TraceId,
  span_id: SpanId,
  operation: PerfOperation,
  /// The caller's span when the trace was continued from another process (`sentry-trace`).
  maybe_parent_span: Option<SpanId>,
  start_ms: f64,
  end_ms: f64,
  status: PerfStatus,
  spans: Vec<PerfSpan>,
  tags: BTreeMap<&'static str, &'static str>,
  measurements: BTreeMap<&'static str, (f64, PerfUnit)>,
}

/// What is measured. A fixed list: the name a backend shows is one of these, never content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PerfOperation {
  /// Signed-in start until the channel list is usable (robustness.md budget: 2 s, web 1.5 s).
  StartToChannelList,
  /// Opening a channel until its messages show (budget p95: 100 ms, mobile 150 ms).
  ChannelSwitch,
  /// Sending a message until the server acknowledged it.
  MessageSend,
  /// Catching up after the connection came back (gateway resume or full resync).
  Resync,
  FileUpload,
  FileDownload,
  /// Starting or joining a call until the media is connected.
  CallSetup,
  /// An MLS commit (adding or removing members, key updates) until the server took it.
  MlsCommit,
  /// A conversation opened (or the app back from a push) until its newest server message is in the
  /// store and shown: catches "the push came, the chat lacks it" behind a fast switch.
  CatchUp,
  /// A page of older history asked for while the reader scrolls back, until its rows show; the
  /// longest frame after they arrived says whether the list stalled on them.
  OlderHistoryPage,
  /// The reader scrolling back in a conversation, from the first scroll up until it rests: how
  /// smooth it was, and how often the list had to move the scroll position under the reader
  /// (#1595).
  ScrollBack,
  /// A root span of the trace layer kept for being slow, failed or sampled (docs/tracing.md):
  /// its operation and description, both from the code.
  Traced {
    op: &'static str,
    name: &'static str,
  },
}

/// How an operation ended (the protocol's span status).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PerfStatus {
  #[default]
  Ok,
  /// The operation failed.
  Error,
  /// The user or the app gave up on it (navigated away, closed the dialog).
  Cancelled,
  /// It ran into a timeout.
  TimedOut,
  /// The server or the network was unavailable.
  Unavailable,
}

/// A step inside a transaction (`http.client` / `GET /api/v1/channels/{id}/messages`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PerfSpan {
  /// The protocol's operation (`http.client`, `db`, `crypto`, `ui.render`).
  pub op: &'static str,
  /// What it did: a route template or a step's name, never a concrete path or ID. Text from
  /// elsewhere (the desktop shell's step names) passes [`PerfSpan::code_name`].
  pub description: Cow<'static, str>,
  pub start_ms: f64,
  pub end_ms: f64,
  pub status: PerfStatus,
  /// Its own ID and its parent's, for a span from the trace layer (docs/tracing.md); `None`: a
  /// direct child of the transaction with a derived ID.
  pub maybe_ids: Option<(SpanId, SpanId)>,
  /// Numbers from the code (`queued_ms`, `busy_ms`, `status`), as the span's data.
  pub data: Vec<(&'static str, f64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PerfUnit {
  Millisecond,
  Byte,
  /// A count (messages, members).
  None,
}

/// A trace: 16 random bytes, 32 hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TraceId([u8; 16]);

/// A span: 8 random bytes, 16 hex digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpanId([u8; 8]);

impl Transaction {
  /// A transaction that started at `start_ms` (Unix time in milliseconds, with a fraction: the
  /// shells measure with a monotonic clock and add it to the start). `random` comes from the
  /// platform's secure random source: 16 bytes for the event, 16 for the trace, 8 for the span.
  pub fn start(operation: PerfOperation, start_ms: f64, random: [u8; 40]) -> Self {
    let mut event = [0; 16];
    let mut trace = [0; 16];
    let mut span = [0; 8];
    event.copy_from_slice(&random[..16]);
    trace.copy_from_slice(&random[16..32]);
    span.copy_from_slice(&random[32..]);
    Self {
      event_id: EventId::from_random_bytes(event),
      trace_id: TraceId(trace),
      span_id: SpanId(span),
      operation,
      maybe_parent_span: None,
      start_ms,
      end_ms: start_ms,
      status: PerfStatus::Ok,
      spans: Vec::new(),
      tags: BTreeMap::new(),
      measurements: BTreeMap::new(),
    }
  }

  /// A root span of the trace layer as a transaction of its own (docs/tracing.md): its
  /// descendants and the spans that blocked it become its spans, its phases its measurements.
  /// `event_random`: 16 bytes from the platform's secure random source.
  pub fn from_trace(tree: &TraceTree, event_random: [u8; 16]) -> Self {
    let root = &tree.root;
    let mut transaction = Self {
      event_id: EventId::from_random_bytes(event_random),
      trace_id: root.trace_id,
      span_id: root.span_id,
      operation: PerfOperation::Traced {
        op: root.op,
        name: root.description,
      },
      maybe_parent_span: root.maybe_parent,
      start_ms: root.start_ms,
      end_ms: root.end_ms.max(root.start_ms),
      status: root.status,
      spans: Vec::new(),
      tags: BTreeMap::new(),
      measurements: BTreeMap::new(),
    };
    transaction.set_tag("trace.keep", tree.reason.as_str());
    if let Some(class) = root.maybe_error {
      transaction.set_tag("error", class);
    }
    for (name, value) in span_numbers(root) {
      transaction.set_measurement(name, value, PerfUnit::of_field(name));
    }
    for span in &tree.spans {
      transaction.add_span(PerfSpan::from_closed(
        span,
        span.maybe_parent.unwrap_or(root.span_id),
      ));
    }
    for span in &tree.blocked_by {
      let mut blocking = PerfSpan::from_closed(span, root.span_id);
      blocking.data.push(("blocked_by", 1.0));
      transaction.add_span(blocking);
    }
    transaction
  }

  /// Adds a closed span of the trace layer as a step: its own ID, under this transaction.
  pub fn add_trace_span(&mut self, span: &ClosedSpan) {
    let step = PerfSpan::from_closed(span, self.span_id);
    self.add_span(step);
  }

  /// Adds a step; beyond [`MAX_SPANS`] steps it is left out.
  pub fn add_span(&mut self, span: PerfSpan) {
    if self.spans.len() < MAX_SPANS {
      self.spans.push(span);
    }
  }

  /// A tag the backend can filter by (`source` = `cache`); beyond [`MAX_TAGS`] it is left out.
  pub fn set_tag(&mut self, key: &'static str, value: &'static str) {
    if self.tags.len() < MAX_TAGS || self.tags.contains_key(key) {
      self.tags.insert(key, value);
    }
  }

  /// A number (bytes moved, messages shown); beyond [`MAX_MEASUREMENTS`] it is left out.
  pub fn set_measurement(&mut self, name: &'static str, value: f64, unit: PerfUnit) {
    if value.is_finite()
      && (self.measurements.len() < MAX_MEASUREMENTS || self.measurements.contains_key(name))
    {
      self.measurements.insert(name, (value, unit));
    }
  }

  /// Ends it at `end_ms` (never before its start) with `status`.
  pub fn finish(mut self, end_ms: f64, status: PerfStatus) -> Self {
    self.end_ms = end_ms.max(self.start_ms);
    self.status = status;
    self
  }

  pub fn event_id(&self) -> EventId {
    self.event_id
  }

  pub fn operation(&self) -> PerfOperation {
    self.operation
  }

  /// When it started, Unix time in milliseconds.
  pub fn start_ms(&self) -> f64 {
    self.start_ms
  }

  /// When it ended, Unix time in milliseconds.
  pub fn end_ms(&self) -> f64 {
    self.end_ms
  }

  pub fn duration_ms(&self) -> f64 {
    self.end_ms - self.start_ms
  }

  /// The envelope (header, item header, payload) as the backend takes it.
  pub fn to_envelope(
    &self,
    app: &AppInfo,
    environment: &Environment,
    installation: InstallationId,
  ) -> String {
    let payload = self.payload(app, environment, installation).to_string();
    let header = json!({ "event_id": self.event_id.to_string() });
    let item = json!({ "type": "transaction", "length": payload.len() });
    format!("{header}\n{item}\n{payload}\n")
  }

  fn payload(&self, app: &AppInfo, environment: &Environment, installation: InstallationId) -> Value {
    let mut tags: BTreeMap<&str, &str> = self.tags.clone();
    tags.insert("app.platform", app.platform.as_str());
    let mut os = json!({ "name": app.os_name });
    if let Some(version) = &app.maybe_os_version {
      os["version"] = json!(version);
    }
    let trace_id = self.trace_id.to_string();
    let spans: Vec<Value> = self
      .spans
      .iter()
      .enumerate()
      .map(|(index, span)| {
        let (span_id, parent) = span
          .maybe_ids
          .unwrap_or((self.span_id.child(index), self.span_id));
        let mut value = json!({
          "trace_id": trace_id,
          "span_id": span_id.to_string(),
          "parent_span_id": parent.to_string(),
          "op": span.op,
          "description": span.description.as_ref(),
          "start_timestamp": seconds_f64(span.start_ms.max(self.start_ms)),
          "timestamp": seconds_f64(span.end_ms.clamp(self.start_ms, self.end_ms)),
          "status": span.status.as_str(),
        });
        if !span.data.is_empty() {
          let data: BTreeMap<&str, f64> = span.data.iter().copied().collect();
          value["data"] = json!(data);
        }
        value
      })
      .collect();
    let measurements: BTreeMap<&str, Value> = self
      .measurements
      .iter()
      .map(|(name, (value, unit))| (*name, json!({ "value": value, "unit": unit.as_str() })))
      .collect();
    let mut payload = json!({
      "type": "transaction",
      "event_id": self.event_id.to_string(),
      "transaction": self.operation.name(),
      "transaction_info": { "source": "custom" },
      "start_timestamp": seconds_f64(self.start_ms),
      "timestamp": seconds_f64(self.end_ms),
      "platform": "native",
      "release": format!("{RELEASE_PREFIX}{}", app.version),
      "environment": environment.as_str(),
      "tags": tags,
      "user": { "id": installation.to_string() },
      "contexts": {
        "trace": {
          "trace_id": trace_id,
          "span_id": self.span_id.to_string(),
          "op": self.operation.op(),
          "status": self.status.as_str(),
        },
        "os": os,
        "app": { "app_version": app.version },
      },
      "spans": spans,
      "measurements": measurements,
    });
    if let Some(parent) = self.maybe_parent_span {
      payload["contexts"]["trace"]["parent_span_id"] = json!(parent.to_string());
    }
    payload
  }
}

impl PerfSpan {
  /// A closed span of the trace layer under `parent`, with its phases and numbers as data.
  pub fn from_closed(span: &ClosedSpan, parent: SpanId) -> Self {
    Self {
      op: span.op,
      description: Cow::Borrowed(span.description),
      start_ms: span.start_ms,
      end_ms: span.end_ms,
      status: span.status,
      maybe_ids: Some((span.span_id, parent)),
      data: span_numbers(span),
    }
  }

  /// A name that came over a boundary (the desktop shell): kept only while it looks like a name
  /// in the code or a route template (`trace_fields::checked_description`), otherwise `other`.
  pub fn code_name(name: &str) -> Cow<'static, str> {
    Cow::Borrowed(super::trace_fields::checked_description(name))
  }
}

impl PerfOperation {
  /// The transaction's name in the backend.
  pub fn name(self) -> &'static str {
    match self {
      Self::StartToChannelList => "start.channel_list",
      Self::ChannelSwitch => "channel.switch",
      Self::MessageSend => "message.send",
      Self::Resync => "sync.reconnect",
      Self::FileUpload => "file.upload",
      Self::FileDownload => "file.download",
      Self::CallSetup => "call.setup",
      Self::MlsCommit => "mls.commit",
      Self::CatchUp => "conversation.catch_up",
      Self::OlderHistoryPage => "history.older_page",
      Self::ScrollBack => "feed.scroll_back",
      Self::Traced { name, .. } => name,
    }
  }

  /// The protocol's operation, which backends group and chart by.
  pub fn op(self) -> &'static str {
    match self {
      Self::StartToChannelList => "app.start",
      Self::ChannelSwitch => "navigation",
      Self::MessageSend | Self::MlsCommit => "task",
      Self::Resync => "sync",
      Self::FileUpload | Self::FileDownload => "file",
      Self::CallSetup => "call",
      Self::CatchUp => "sync",
      Self::OlderHistoryPage => "ui.load",
      Self::ScrollBack => "ui.scroll",
      Self::Traced { op, .. } => op,
    }
  }
}

impl PerfStatus {
  /// The protocol's name back to the status; anything else is an error.
  pub(super) fn from_protocol(name: &str) -> Self {
    match name {
      "ok" => Self::Ok,
      "cancelled" => Self::Cancelled,
      "deadline_exceeded" => Self::TimedOut,
      "unavailable" => Self::Unavailable,
      _ => Self::Error,
    }
  }

  pub(super) fn as_str(self) -> &'static str {
    match self {
      Self::Ok => "ok",
      Self::Error => "internal_error",
      Self::Cancelled => "cancelled",
      Self::TimedOut => "deadline_exceeded",
      Self::Unavailable => "unavailable",
    }
  }
}

/// A span's phases, CPU time and numbers, by name.
fn span_numbers(span: &ClosedSpan) -> Vec<(&'static str, f64)> {
  let mut numbers = Vec::with_capacity(4 + span.data.len());
  if let Some(phases) = span.maybe_phases {
    numbers.push(("queued_ms", phases.queued_ms));
    numbers.push(("busy_ms", phases.busy_ms));
    numbers.push(("idle_ms", phases.idle_ms));
  }
  if let Some(cpu_ms) = span.maybe_cpu_ms {
    numbers.push(("cpu_ms", cpu_ms));
  }
  numbers.extend(span.data.iter().copied());
  numbers
}

impl KeepReason {
  pub(super) fn as_str(self) -> &'static str {
    match self {
      Self::Failed => "failed",
      Self::Slow => "slow",
      Self::Sampled => "sampled",
    }
  }
}

impl PerfUnit {
  /// The unit a field's name says (`*_ms`, `*_bytes`), else a count.
  fn of_field(name: &str) -> Self {
    if name.ends_with("_ms") {
      Self::Millisecond
    } else if name.ends_with("_bytes") {
      Self::Byte
    } else {
      Self::None
    }
  }

  fn as_str(self) -> &'static str {
    match self {
      Self::Millisecond => "millisecond",
      Self::Byte => "byte",
      Self::None => "none",
    }
  }
}

impl TraceId {
  pub(super) fn from_u128(value: u128) -> Self {
    Self(value.to_be_bytes())
  }

  /// 32 hex digits, as in a `sentry-trace` header; `None` for anything else or all zeros.
  pub fn parse(text: &str) -> Option<Self> {
    let value = parse_hex(text, 32)?;
    (value != 0).then(|| Self::from_u128(value))
  }
}

impl SpanId {
  pub(super) fn from_u64(value: u64) -> Self {
    Self(value.to_be_bytes())
  }

  /// 16 hex digits, as in a `sentry-trace` header; `None` for anything else or all zeros.
  pub fn parse(text: &str) -> Option<Self> {
    let value = u64::try_from(parse_hex(text, 16)?).ok()?;
    (value != 0).then(|| Self::from_u64(value))
  }

  /// A child span's ID: unique inside the trace, derived from the parent so no more random bytes
  /// are needed (span IDs only have to be unique within their trace).
  fn child(self, index: usize) -> Self {
    let mut bytes = self.0;
    let [high, low] = u16::try_from(index + 1).unwrap_or(u16::MAX).to_be_bytes();
    bytes[6] ^= high ^ 0x80;
    bytes[7] ^= low;
    Self(bytes)
  }
}

fn parse_hex(text: &str, digits: usize) -> Option<u128> {
  if text.len() != digits || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return None;
  }
  u128::from_str_radix(text, 16).ok()
}

impl std::fmt::Display for TraceId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&hex(&self.0))
  }
}

impl std::fmt::Display for SpanId {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str(&hex(&self.0))
  }
}

#[cfg(test)]
mod tests {
  use std::collections::HashSet;

  use super::super::AppPlatform;
  use super::*;

  #[test]
  fn envelope_has_a_transaction_item_with_trace_context_and_spans() {
    let envelope = sample().to_envelope(
      &app(),
      &Environment::default(),
      InstallationId::from_random_bytes([1; 16]),
    );
    let lines: Vec<&str> = envelope.lines().collect();
    assert_eq!(lines.len(), 3);
    let item: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(item["type"], json!("transaction"));
    assert_eq!(item["length"], json!(lines[2].len()));
    let payload: Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(payload["type"], json!("transaction"));
    assert_eq!(payload["transaction"], json!("channel.switch"));
    assert_eq!(payload["release"], json!("teamcraft@1.0.9"));
    assert_eq!(payload["environment"], json!("production"));
    assert_eq!(payload["user"], json!({ "id": "01".repeat(16) }));
    assert_eq!(payload["tags"]["app.platform"], json!("web"));
    assert_eq!(payload["tags"]["source"], json!("network"));
    assert_eq!(payload["start_timestamp"], json!(1_700_000_000.0));
    assert_eq!(payload["timestamp"], json!(1_700_000_000.25));
    let trace = &payload["contexts"]["trace"];
    assert_eq!(trace["trace_id"], json!("02".repeat(16)));
    assert_eq!(trace["span_id"], json!("03".repeat(8)));
    assert_eq!(trace["op"], json!("navigation"));
    assert_eq!(trace["status"], json!("ok"));
    let span = &payload["spans"][0];
    assert_eq!(span["parent_span_id"], json!("03".repeat(8)));
    assert_eq!(span["trace_id"], json!("02".repeat(16)));
    assert_eq!(span["op"], json!("http.client"));
    assert_eq!(span["description"], json!("GET /api/v1/channels/{id}/messages"));
    assert_eq!(span["status"], json!("internal_error"));
    assert_eq!(
      payload["measurements"]["messages"],
      json!({ "value": 50.0, "unit": "none" })
    );
    for field in ["server_name", "request", "extra", "modules", "breadcrumbs"] {
      assert!(payload.get(field).is_none(), "{field}");
    }
  }

  #[test]
  fn spans_are_clamped_into_the_transaction() {
    let mut transaction = Transaction::start(PerfOperation::MessageSend, 1_000.0, [0; 40]);
    transaction.add_span(PerfSpan {
      op: "ui",
      description: "late".into(),
      start_ms: 500.0,
      end_ms: 9_000.0,
      status: PerfStatus::Ok,
      ..Default::default()
    });
    let transaction = transaction.finish(2_000.0, PerfStatus::Ok);
    let payload = transaction.payload(
      &app(),
      &Environment::default(),
      InstallationId::from_random_bytes([0; 16]),
    );
    assert_eq!(payload["spans"][0]["start_timestamp"], json!(1.0));
    assert_eq!(payload["spans"][0]["timestamp"], json!(2.0));
    // An end before the start (a clock step) is no negative duration.
    let backwards =
      Transaction::start(PerfOperation::Resync, 5_000.0, [0; 40]).finish(4_000.0, PerfStatus::Ok);
    assert_eq!(backwards.duration_ms(), 0.0);
  }

  #[test]
  fn spans_tags_and_measurements_are_bounded() {
    let mut transaction = Transaction::start(PerfOperation::Resync, 0.0, [7; 40]);
    for _ in 0..MAX_SPANS + 10 {
      transaction.add_span(PerfSpan {
        op: "x",
        description: "y".into(),
        start_ms: 0.0,
        end_ms: 1.0,
        status: PerfStatus::Ok,
        ..Default::default()
      });
    }
    const KEYS: [&str; 12] = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
    for key in KEYS {
      transaction.set_tag(key, "v");
      transaction.set_measurement(key, 1.0, PerfUnit::None);
    }
    transaction.set_measurement("a", f64::NAN, PerfUnit::None);
    assert_eq!(transaction.spans.len(), MAX_SPANS);
    assert_eq!(transaction.tags.len(), MAX_TAGS);
    assert_eq!(transaction.measurements.len(), MAX_MEASUREMENTS);
    assert_eq!(transaction.measurements["a"].0, 1.0, "NaN is no measurement");
    let payload = transaction.payload(
      &app(),
      &Environment::default(),
      InstallationId::from_random_bytes([0; 16]),
    );
    let ids: HashSet<String> = payload["spans"]
      .as_array()
      .unwrap()
      .iter()
      .map(|span| span["span_id"].as_str().unwrap().to_owned())
      .chain([payload["contexts"]["trace"]["span_id"]
        .as_str()
        .unwrap()
        .to_owned()])
      .collect();
    assert_eq!(ids.len(), MAX_SPANS + 1, "span IDs are unique in the trace");
  }

  #[test]
  fn names_from_the_shell_must_look_like_code() {
    assert_eq!(
      PerfSpan::code_name("catch_up_conversation"),
      "catch_up_conversation"
    );
    assert_eq!(PerfSpan::code_name("migrate_to_v5"), "migrate_to_v5");
    for leaky in ["Alice Example", "a@example.com", "/home/x", "", &"x".repeat(49)] {
      assert_eq!(PerfSpan::code_name(leaky), "other", "{leaky}");
    }
  }

  #[test]
  fn every_operation_has_a_distinct_name() {
    let all = [
      PerfOperation::StartToChannelList,
      PerfOperation::ChannelSwitch,
      PerfOperation::MessageSend,
      PerfOperation::Resync,
      PerfOperation::FileUpload,
      PerfOperation::FileDownload,
      PerfOperation::CallSetup,
      PerfOperation::MlsCommit,
      PerfOperation::CatchUp,
      PerfOperation::OlderHistoryPage,
      PerfOperation::ScrollBack,
    ];
    let names: HashSet<&str> = all.iter().map(|operation| operation.name()).collect();
    assert_eq!(names.len(), all.len());
  }

  fn sample() -> Transaction {
    let mut random = [0; 40];
    random[16..32].fill(2);
    random[32..].fill(3);
    let mut transaction = Transaction::start(PerfOperation::ChannelSwitch, 1_700_000_000_000.0, random);
    transaction.add_span(PerfSpan {
      op: "http.client",
      description: "GET /api/v1/channels/{id}/messages".into(),
      start_ms: 1_700_000_000_010.0,
      end_ms: 1_700_000_000_070.0,
      status: PerfStatus::Error,
      ..Default::default()
    });
    transaction.set_tag("source", "network");
    transaction.set_measurement("messages", 50.0, PerfUnit::None);
    transaction.finish(1_700_000_000_250.0, PerfStatus::Ok)
  }

  fn app() -> AppInfo {
    AppInfo {
      version: "1.0.9".to_owned(),
      platform: AppPlatform::Web,
      os_name: "Linux".to_owned(),
      maybe_os_version: None,
    }
  }
}
