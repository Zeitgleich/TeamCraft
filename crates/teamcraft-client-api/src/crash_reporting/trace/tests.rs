use std::sync::Mutex;

use tracing_subscriber::layer::SubscriberExt as _;

use super::*;
use crate::crash_reporting::{
  AppInfo, AppPlatform, Environment, InstallationId, PerfOperation, SampleRate, Transaction,
};

/// The layer's state is per process: tests that use it run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// What a careless caller could put into a span; none of it may leave.
const SECRETS: [&str; 10] = [
  "Meet me at the station",
  "Alice Example",
  "quarterly-report.pdf",
  "alice@example.com",
  "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
  "hunter2",
  "/home/alice/Documents",
  "7319482025",
  "9f86d081884c7d659a2feaa0c55ad015",
  "conv-share-token",
];

#[test]
fn no_span_field_can_carry_content() {
  let _serial = serial();
  let tree = traced_tree(|| {
    let root = tracing::info_span!(
      target: "teamcraft_test",
      "queue.read",
      name = "Meet me at the station",
      kind = %"Alice Example",
      job = ?"quarterly-report.pdf",
      route = "/api/v1/users/alice@example.com/messages?q=hunter2#/home/alice/Documents",
      method = "GET",
      conversation_id = 7_319_482_025_u64,
      message = "Meet me at the station",
      token = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl",
      status = 500_u64,
      error = "hunter2 at /home/alice/Documents",
    );
    let _entered = root.enter();
    let child = tracing::info_span!(
      target: "teamcraft_test",
      "http.client",
      method = "post",
      route = "https://chat.example.com/api/v1/links/conv-share-token/9f86d081884c7d659a2feaa0c55ad015",
      body_bytes = 12_u64,
      user = "Alice Example",
      file_name = "quarterly-report.pdf",
    );
    child.in_scope(|| {});
    tracing::info!(target: "teamcraft_test", body = "Meet me at the station", "an event is not a span");
  });
  let envelope = Transaction::from_trace(&tree, [7; 16]).to_envelope(
    &app(),
    &Environment::default(),
    InstallationId::from_random_bytes([1; 16]),
  );
  let breadcrumbs = breadcrumb_lines(0.0).join("\n");
  for secret in SECRETS {
    assert!(!envelope.contains(secret), "{secret} in {envelope}");
    assert!(!breadcrumbs.contains(secret), "{secret} in {breadcrumbs}");
  }
  assert!(envelope.contains("\"queue.read\""), "{envelope}");
  assert!(envelope.contains("POST /api/v1/links/{token}/{id}"), "{envelope}");
  assert!(envelope.contains("\"body_bytes\":12.0"), "{envelope}");
}

mod phases {
  use super::*;

  #[test]
  fn a_queued_job_waited_then_worked_then_waited_again() {
    let mut state = SpanState::new("queue.read", None);
    state.start_mono_ms = 0.0;
    state.enter(40.0, None);
    state.exit(50.0, None);
    state.enter(90.0, None);
    state.exit(100.0, None);
    let closed = state.close(120.0);
    assert_eq!(
      closed.maybe_phases,
      Some(Phases {
        queued_ms: 40.0,
        busy_ms: 20.0,
        idle_ms: 60.0,
      })
    );
    assert_eq!(closed.duration_ms(), 120.0);
  }

  #[test]
  fn nested_entries_count_once_and_cpu_adds_up() {
    let mut state = SpanState::new("queue.engine", None);
    state.start_mono_ms = 0.0;
    state.enter(0.0, Some(Duration::from_millis(100)));
    state.enter(5.0, Some(Duration::from_millis(101)));
    state.exit(8.0, Some(Duration::from_millis(102)));
    state.exit(10.0, Some(Duration::from_millis(104)));
    let closed = state.close(10.0);
    assert_eq!(closed.maybe_phases.map(|phases| phases.busy_ms), Some(10.0));
    assert_eq!(closed.maybe_cpu_ms, Some(4.0));
  }

  #[test]
  fn a_span_never_entered_has_no_phases() {
    let state = SpanState::new("core.step", None);
    assert_eq!(state.close(mono_ms()).maybe_phases, None);
  }
}

mod keeping {
  use super::*;

  #[test]
  fn a_failed_root_is_kept_with_its_children_in_one_trace() {
    let _serial = serial();
    let tree = traced_tree(|| {
      let root = tracing::info_span!(
        target: "teamcraft_test",
        "queue.engine",
        name = "send_message",
        error = tracing::field::Empty,
      );
      root.in_scope(|| {
        tracing::info_span!(target: "teamcraft_test", "http.client", method = "POST", route = "/api/v1/x/1")
          .in_scope(|| {});
        root.record("error", "timeout");
      });
    });
    assert_eq!(tree.reason, KeepReason::Failed);
    assert_eq!(tree.root.status, PerfStatus::TimedOut);
    assert_eq!(tree.root.description, "send_message");
    assert_eq!(tree.spans.len(), 1);
    assert_eq!(tree.spans[0].trace_id, tree.root.trace_id);
    assert_eq!(tree.spans[0].maybe_parent, Some(tree.root.span_id));
    assert_eq!(tree.spans[0].description, "POST /api/v1/x/{id}");
  }

  #[test]
  fn at_full_rate_every_root_worth_a_look_is_sampled() {
    let _serial = serial();
    configure(TraceSettings {
      is_on: true,
      trace_rate: SampleRate::ALL,
      slow_ms: 0,
    });
    let mut root = closed("queue.read", "full_rate_sample", 0.0, MIN_SAMPLED_MS, None);
    assert_eq!(keep_reason(&root), Some(KeepReason::Sampled));
    root.end_ms = MIN_SAMPLED_MS / 2.0;
    assert_eq!(keep_reason(&root), None, "too short to say anything");
    configure(settings(false));
  }

  #[test]
  fn nothing_is_collected_while_off() {
    let _serial = serial();
    configure(settings(false));
    let _ = take_kept();
    with_layer(|| {
      tracing::info_span!(target: "teamcraft_test", "queue.read", name = "while_off", error = true)
        .in_scope(|| {});
    });
    assert!(take_kept().is_empty());
    assert!(
      recent(0.0, f64::MAX)
        .iter()
        .all(|span| span.description != "while_off")
    );
  }

  #[test]
  fn slow_roots_are_all_kept_except_in_a_storm_of_one() {
    let mut shared = Shared::new();
    let kept = (0..50).filter(|_| shared.allows_slow("messages", 0.0)).count();
    assert_eq!(kept, MAX_SLOW_PER_DESCRIPTION_PER_MINUTE as usize);
    assert!(
      shared.allows_slow("conversations", 1_000.0),
      "another one is not held up"
    );
    assert!(
      shared.allows_slow("messages", MINUTE_MS + 1.0),
      "the next minute starts over"
    );
    let many = (0..1_000)
      .filter(|index| shared.allows_slow(if index % 2 == 0 { "a" } else { "b" }, 0.0))
      .count();
    assert_eq!(
      many,
      2 * MAX_SLOW_PER_DESCRIPTION_PER_MINUTE as usize,
      "no cap per process"
    );
  }

  #[test]
  fn sampled_roots_are_capped_per_minute() {
    let mut shared = Shared::new();
    let allowed = (0..50).filter(|_| shared.allows_sampled(10.0)).count();
    assert_eq!(allowed, MAX_SAMPLED_PER_MINUTE as usize);
    assert!(shared.allows_sampled(MINUTE_MS + 10.0));
  }

  #[test]
  fn a_waiting_root_names_what_ran_before_it() {
    let _serial = serial();
    let blocking = closed("queue.read", "conversations", 1_000.0, 1_400.0, None);
    let other_queue = closed("queue.engine", "sync", 1_000.0, 1_400.0, None);
    push_ring(&blocking);
    push_ring(&other_queue);
    let mut waiting = closed("queue.read", "messages", 1_000.0, 1_600.0, None);
    waiting.maybe_phases = Some(Phases {
      queued_ms: 500.0,
      busy_ms: 100.0,
      idle_ms: 0.0,
    });
    let found = blocked_by(&waiting);
    assert!(found.iter().any(|span| span.description == "conversations"));
    assert!(found.iter().all(|span| span.op == "queue.read"));
  }
}

mod propagation {
  use super::*;

  #[test]
  fn the_header_names_the_current_span_and_the_server_continues_it() {
    let _serial = serial();
    configure(settings(true));
    let _ = take_kept();
    let mut maybe_header = None;
    with_layer(|| {
      tracing::info_span!(target: "teamcraft_test", "http.client", name = "header_client", error = true)
        .in_scope(|| maybe_header = sentry_trace_header());
    });
    let header = maybe_header.expect("a header inside a collected span");
    let client = take_kept().pop().expect("the failed client span");
    assert_eq!(
      header,
      format!("{}-{}", client.root.trace_id, client.root.span_id)
    );

    with_layer(|| {
      tracing::info_span!(target: "teamcraft_test", "http.server", name = "header_server", sentry_trace = header.as_str(), error = true)
        .in_scope(|| {});
    });
    let server = take_kept().pop().expect("the failed server span");
    assert_eq!(server.root.trace_id, client.root.trace_id);
    assert_eq!(server.root.maybe_parent, Some(client.root.span_id));
    assert!(sentry_trace_header().is_none());
  }

  #[test]
  fn a_malformed_header_starts_a_trace_of_its_own() {
    let mut state = SpanState::new("http.server", None);
    let own = state.trace_id;
    state.continue_trace("not-a-header");
    state.continue_trace("00000000000000000000000000000000-0000000000000000");
    assert_eq!(state.trace_id, own);
    assert_eq!(state.maybe_parent, None);
  }
}

mod sql {
  use super::*;

  #[test]
  fn a_statement_becomes_a_child_without_its_literals() {
    let _serial = serial();
    let tree = traced_tree(|| {
      tracing::info_span!(target: "teamcraft_test", "http.server", name = "sql_test", error = true).in_scope(
        || {
          tracing::debug!(
            target: "sqlx::query",
            summary = "SELECT body FROM messages WHERE author = 'Alice Example'",
            rows_returned = 3_u64,
            elapsed_secs = 0.25,
          );
        },
      );
    });
    let statement = tree
      .spans
      .iter()
      .find(|span| span.op == "db.sql")
      .expect("a db.sql span");
    assert_eq!(
      statement.description,
      "SELECT body FROM messages WHERE author = ?"
    );
    assert_eq!(statement.data, vec![("rows", 3.0)]);
    assert!((statement.duration_ms() - 250.0).abs() < 1.0);
  }
}

mod open {
  use super::*;

  #[test]
  fn a_hanging_span_is_listed_as_open_until_it_ends() {
    let _serial = serial();
    configure(settings(true));
    with_layer(|| {
      let stuck = tracing::info_span!(target: "teamcraft_test", "queue.read", name = "stuck_job");
      assert!(
        open_spans()
          .iter()
          .any(|span| span.description == "stuck_job" && !span.has_started)
      );
      let lines = breadcrumb_lines(f64::MAX).join("\n");
      assert!(lines.contains("open queue.read stuck_job queued"), "{lines}");
      drop(stuck);
      assert!(open_spans().iter().all(|span| span.description != "stuck_job"));
    });
  }
}

/// Measures what a span costs (robustness.md, "Performance and resources"): the hot paths make a
/// few spans per user action, so a few microseconds each are noise against the 100 ms budgets.
#[test]
fn a_span_costs_little() {
  const SPANS: u32 = 2_000;
  let _serial = serial();
  let per_span = |is_on: bool| {
    configure(settings(is_on));
    let mut took = Duration::ZERO;
    with_layer(|| {
      let started = std::time::Instant::now();
      for _ in 0..SPANS {
        tracing::info_span!(target: "teamcraft_test", "queue.read", name = "cost").in_scope(|| {});
      }
      took = started.elapsed();
    });
    took / SPANS
  };
  let off = per_span(false);
  let on = per_span(true);
  let _ = take_kept();
  // The numbers for docs/tracing.md ("Cost"): `cargo test --release -- a_span_costs_little --nocapture`.
  println!("a span costs {off:?} with collection off, {on:?} with it on");
  // Generous bounds for debug builds on shared CI machines; release is far below.
  assert!(off < Duration::from_micros(20), "off: {off:?} per span");
  assert!(on < Duration::from_micros(200), "on: {on:?} per span");
}

fn serial() -> std::sync::MutexGuard<'static, ()> {
  SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs `body` with collection on and returns the one tree it kept.
fn traced_tree(body: impl FnOnce()) -> TraceTree {
  configure(settings(true));
  let _ = take_kept();
  with_layer(body);
  let mut kept = take_kept();
  assert_eq!(kept.len(), 1, "{kept:?}");
  kept.remove(0)
}

fn with_layer(body: impl FnOnce()) {
  let subscriber = tracing_subscriber::registry().with(layer());
  tracing::subscriber::with_default(subscriber, body);
}

fn settings(is_on: bool) -> TraceSettings {
  TraceSettings {
    is_on,
    trace_rate: SampleRate::NONE,
    slow_ms: 0,
  }
}

fn closed(
  op: &'static str,
  description: &'static str,
  start_ms: f64,
  end_ms: f64,
  maybe_parent: Option<SpanId>,
) -> ClosedSpan {
  ClosedSpan {
    op,
    description,
    trace_id: new_trace_id(),
    span_id: new_span_id(),
    maybe_parent,
    start_ms,
    end_ms,
    maybe_phases: None,
    maybe_cpu_ms: None,
    status: PerfStatus::Ok,
    maybe_error: None,
    data: Vec::new(),
  }
}

fn app() -> AppInfo {
  AppInfo {
    version: "1.0.9".to_owned(),
    platform: AppPlatform::Web,
    os_name: "Linux".to_owned(),
    maybe_os_version: None,
  }
}

#[test]
fn a_traced_operation_is_named_by_its_root() {
  let operation = PerfOperation::Traced {
    op: "queue.read",
    name: "messages",
  };
  assert_eq!(operation.name(), "messages");
  assert_eq!(operation.op(), "queue.read");
}

mod boundary {
  use super::*;

  #[test]
  fn a_tree_crosses_from_the_shell_to_the_page_unchanged() {
    let mut root = closed("queue.engine", "send_message", 1_000.0, 1_900.0, None);
    root.maybe_phases = Some(Phases {
      queued_ms: 600.0,
      busy_ms: 100.0,
      idle_ms: 200.0,
    });
    root.data = vec![("depth", 4.0)];
    let child = closed(
      "http.client",
      "POST /api/v1/conversations/{id}/messages",
      1_650.0,
      1_850.0,
      Some(root.span_id),
    );
    let tree = TraceTree {
      root,
      spans: vec![child],
      blocked_by: Vec::new(),
      reason: KeepReason::Slow,
    };
    let json = serde_json::to_string(&WireTraceTree::from(&tree)).unwrap();
    let back: WireTraceTree = serde_json::from_str(&json).unwrap();
    assert_eq!(back.into_tree(), Some(tree));
  }

  #[test]
  fn a_shell_can_not_widen_what_the_page_sends() {
    let mut wire = WireSpan::from(&closed("queue.read", "messages", 1.0, 2.0, None));
    wire.description = "Alice Example".to_owned();
    wire.maybe_error = Some("hunter2 at /home/alice".to_owned());
    wire.data = vec![
      ("conversation_id".to_owned(), 7_319_482_025.0),
      ("busy_ms".to_owned(), 1.0),
    ];
    let span = wire.clone().into_span().unwrap();
    assert_eq!(span.description, OTHER);
    assert_eq!(span.maybe_error, Some(OTHER));
    assert_eq!(span.data, vec![("busy_ms", 1.0)]);
    wire.op = "Alice Example".to_owned();
    assert_eq!(wire.clone().into_span(), None);
    wire.op = "queue.read".to_owned();
    wire.trace_id = "not hex".to_owned();
    assert_eq!(wire.into_span(), None);
  }
}

/// The sizes behind docs/tracing.md's data volume: a typical kept root (a queue job with a request,
/// its body and a few steps) and a full one (31 descendants, 8 that blocked it).
#[test]
fn a_kept_trace_is_a_few_kilobytes() {
  let envelope_bytes = |children: usize, blocked: usize| {
    let mut root = closed("queue.engine", "send_message", 0.0, 2_000.0, None);
    root.maybe_phases = Some(Phases {
      queued_ms: 500.0,
      busy_ms: 300.0,
      idle_ms: 1_200.0,
    });
    root.maybe_cpu_ms = Some(250.0);
    root.data = vec![("depth", 3.0)];
    let span = |index: usize, parent: SpanId| {
      let mut span = closed(
        "http.client",
        "POST /api/v1/conversations/{id}/messages",
        10.0,
        900.0,
        Some(parent),
      );
      span.data = vec![("status", 201.0), ("body_bytes", 512.0)];
      span.maybe_phases = Some(Phases {
        queued_ms: 0.0,
        busy_ms: 3.0,
        idle_ms: 880.0 + index as f64,
      });
      span
    };
    let tree = TraceTree {
      spans: (0..children).map(|index| span(index, root.span_id)).collect(),
      blocked_by: (0..blocked).map(|index| span(index, root.span_id)).collect(),
      root,
      reason: KeepReason::Sampled,
    };
    Transaction::from_trace(&tree, [7; 16])
      .to_envelope(
        &app(),
        &Environment::default(),
        InstallationId::from_random_bytes([1; 16]),
      )
      .len()
  };
  let typical = envelope_bytes(5, 0);
  let full = envelope_bytes(MAX_CHILDREN, MAX_BLOCKED_BY);
  println!("a kept trace: {typical} bytes typical, {full} bytes full");
  assert!(typical < 4 * 1024, "{typical}");
  assert!(full < 16 * 1024, "{full}");
}
