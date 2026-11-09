//! Timed steps of the core (issue #1521, docs/crash-reports.md "Performance data"): named stretches
//! of work (`gateway_resume`, `catch_up_conversation`, `snapshot_build`) and how long the local
//! store held a thread up (a wait for another thread's group of writes, a group of writes). The apps
//! attach the ones that fall into a measure as its steps: the mobile FFI through an [`Observer`],
//! the web page and the desktop shell through the ring ([`enable_ring`], [`recent`]). Until one of
//! them asks, nothing is timed and a step costs one atomic load.
//!
//! Only static names and times leave this module: no table, key, ID or value.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::Duration;

/// Waits shorter than this are not reported: they explain nothing a person notices.
pub const MIN_REPORTED: Duration = Duration::from_millis(5);
/// Stretches the ring keeps; the oldest drop out. A measure looks back seconds, not minutes.
const RING_CAPACITY: usize = 512;
/// Stretches handed to one measure at most: the longest.
pub const MAX_PER_MEASURE: usize = 24;

static OBSERVER: OnceLock<Observer> = OnceLock::new();
static IS_RING_ON: AtomicBool = AtomicBool::new(false);
static RING: Mutex<VecDeque<Timed>> = Mutex::new(VecDeque::new());

/// Called with each timed stretch; must be cheap and never block.
pub type Observer = fn(Timed);

/// One timed stretch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timed {
  pub kind: TimedKind,
  /// A name from the code (`gateway_resume`); for the store's waits `store_lock` / `write_group`.
  pub name: &'static str,
  /// When it began, Unix time in milliseconds.
  pub started_at_ms: f64,
  pub took: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimedKind {
  /// A named step of the core's work ([`step`]).
  Step,
  /// A thread waited for another thread's group of writes (or reads) to end.
  GroupLock,
  /// A group of writes held the store, from its first write until it committed.
  WriteGroup,
}

/// A step that is timed until it is dropped; `None` while nothing asks for steps.
#[must_use = "the step is timed until this is dropped"]
pub struct Step {
  mark: Mark,
  name: &'static str,
}

/// A moment to time from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mark {
  #[cfg(not(target_arch = "wasm32"))]
  instant: std::time::Instant,
  started_at_ms: f64,
}

/// Times the rest of the scope as `name`: `let _step = timing::step("gateway_resume");`
pub fn step(name: &'static str) -> Option<Step> {
  Mark::now().map(|mark| Step { mark, name })
}

/// `crate::module::function::{{closure}}` → `function`: the function a job or request came from,
/// a name from the code (`std::any::type_name` of a closure it built).
pub fn code_name(type_name: &'static str) -> &'static str {
  let mut rest = type_name;
  while let Some(trimmed) = rest.strip_suffix("::{{closure}}") {
    rest = trimmed;
  }
  // Generic arguments (`read<…>`) go with everything after the first `<`.
  let rest = rest.split('<').next().unwrap_or(rest);
  rest
    .rsplit("::")
    .next()
    .filter(|name| !name.is_empty())
    .unwrap_or("job")
}

/// Installs the observer once per process; later calls change nothing.
pub fn set_observer(observer: Observer) {
  // A second install is ignored on purpose: one observer per process.
  let _ = OBSERVER.set(observer);
}

/// Keeps stretches for [`recent`] from now on (the web page, the desktop shell).
pub fn enable_ring() {
  IS_RING_ON.store(true, Ordering::Relaxed);
}

/// The stretches that overlap `start_ms` to `end_ms` (Unix milliseconds), the longest first, at
/// most [`MAX_PER_MEASURE`], then in time order.
pub fn recent(start_ms: f64, end_ms: f64) -> Vec<Timed> {
  let ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
  let mut overlapping: Vec<Timed> = ring
    .iter()
    .filter(|timed| {
      timed.started_at_ms + duration_ms(timed.took) >= start_ms && timed.started_at_ms <= end_ms
    })
    .copied()
    .collect();
  overlapping.sort_by_key(|timed| std::cmp::Reverse(timed.took));
  overlapping.truncate(MAX_PER_MEASURE);
  overlapping.sort_by(|a, b| a.started_at_ms.total_cmp(&b.started_at_ms));
  overlapping
}

/// Whether anything listens: the store and the steps skip their clock reads otherwise.
fn is_observed() -> bool {
  IS_RING_ON.load(Ordering::Relaxed) || OBSERVER.get().is_some()
}

fn note(timed: Timed) {
  if timed.took < MIN_REPORTED {
    return;
  }
  if let Some(observer) = OBSERVER.get() {
    observer(timed);
  }
  if IS_RING_ON.load(Ordering::Relaxed) {
    let mut ring = RING.lock().unwrap_or_else(PoisonError::into_inner);
    if ring.len() >= RING_CAPACITY {
      ring.pop_front();
    }
    ring.push_back(timed);
  }
}

impl Drop for Step {
  fn drop(&mut self) {
    self.mark.note_named(TimedKind::Step, self.name);
  }
}

impl Mark {
  /// `None` while nothing asks for steps.
  pub(crate) fn now() -> Option<Self> {
    if !is_observed() {
      return None;
    }
    Some(Self {
      #[cfg(not(target_arch = "wasm32"))]
      instant: std::time::Instant::now(),
      started_at_ms: unix_ms(),
    })
  }

  /// The store's waits.
  pub(crate) fn note(self, kind: TimedKind) {
    let name = match kind {
      TimedKind::GroupLock => "store_lock",
      TimedKind::WriteGroup => "write_group",
      TimedKind::Step => "step",
    };
    self.note_named(kind, name);
  }

  fn note_named(&self, kind: TimedKind, name: &'static str) {
    note(Timed {
      kind,
      name,
      started_at_ms: self.started_at_ms,
      took: self.elapsed(),
    });
  }

  fn elapsed(&self) -> Duration {
    #[cfg(not(target_arch = "wasm32"))]
    {
      self.instant.elapsed()
    }
    #[cfg(target_arch = "wasm32")]
    {
      // The browser's clock: whole milliseconds are precise enough for steps a person notices.
      Duration::from_secs_f64(((unix_ms() - self.started_at_ms) / 1_000.0).max(0.0))
    }
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

fn duration_ms(duration: Duration) -> f64 {
  duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_ring_keeps_steps_and_hands_out_the_overlapping_ones() {
    enable_ring();
    note(Timed {
      kind: TimedKind::Step,
      name: "catch_up_conversation",
      started_at_ms: 1_000.0,
      took: Duration::from_millis(500),
    });
    note(Timed {
      kind: TimedKind::Step,
      name: "too_short",
      started_at_ms: 1_000.0,
      took: Duration::from_millis(1),
    });
    let found = recent(1_200.0, 2_000.0);
    assert!(found.iter().any(|timed| timed.name == "catch_up_conversation"));
    assert!(!found.iter().any(|timed| timed.name == "too_short"));
    assert!(
      recent(5_000.0, 6_000.0)
        .iter()
        .all(|timed| timed.name != "catch_up_conversation")
    );
    drop(step("test_step"));
  }

  #[test]
  fn code_names_are_the_function() {
    assert_eq!(
      code_name("teamcraft_ffi::TeamCraft::messages::{{closure}}"),
      "messages"
    );
    assert_eq!(
      code_name("a::b::conversations::{{closure}}::{{closure}}"),
      "conversations"
    );
    assert_eq!(code_name("a::TeamCraft::read<alloc::vec::Vec<u8>>"), "read");
    assert_eq!(code_name(""), "job");
  }
}
