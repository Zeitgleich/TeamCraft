//! Whether the whole process stopped running for a while in the foreground (issue #2225).
//!
//! On 1.0.41 a Pixel reported every thread of the core held for 7 s at once, right after the app
//! came back: the actions' thread, the main read thread and the engine, each with a few
//! milliseconds of CPU, all going on in the same millisecond. The reports blamed whichever job ran
//! longest ("Read lane blocked: act"), but nothing in the core can stop every thread at once. A
//! process the OS doesn't run can.
//!
//! A thread of its own wakes every [`TICK`] while the app is in the foreground. A tick that comes
//! [`MIN_STALL`] or more late means the process got no CPU meanwhile, every thread of it alike: the
//! stall is kept, and a job whose wait or run was mostly inside one is reported as blocked by
//! `process_stall` instead of by the job that happened to hold its thread. In the background the
//! thread sleeps until the app is back: the OS freezes background apps on purpose, and that case is
//! told apart already (`perf_jobs`, issue #2146).
//!
//! Timing is on the monotonic clock (a stall is time that passed without the process running);
//! the stall is kept in Unix milliseconds, like the jobs it is compared with.

use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use crate::logging;
use crate::perf_jobs;

/// How often the watch wakes in the foreground: twice a second costs nothing measurable and finds
/// a stall long before a user would notice one.
const TICK: Duration = Duration::from_millis(500);
/// A tick this late (beyond its due time) is a stall: far above a busy scheduler's delays, at the
/// scale of the slow-job threshold.
const MIN_STALL: Duration = Duration::from_secs(1);
/// Stalls kept at most; reports look back seconds, not minutes.
const MAX_STALLS: usize = 32;
/// The name a stall is reported under, as the blocker of a lane.
pub(crate) const PROCESS_STALL: &str = "process_stall";

static WATCH: Watch = Watch::new();
static STARTED: OnceLock<()> = OnceLock::new();

/// The watch's state, shared by its thread and the jobs that ask.
struct Watch {
  state: Mutex<WatchState>,
  /// Raised when the app comes to the foreground.
  foreground: Condvar,
}

struct WatchState {
  is_foreground: bool,
  stalls: Stalls,
}

/// The stalls found, and the last tick a late one is measured from.
#[derive(Debug, Default)]
struct Stalls {
  done: VecDeque<Stall>,
  /// The last tick, as long as the watch runs.
  maybe_last_tick: Option<Tick>,
}

/// A tick of the watch, on the monotonic clock.
#[derive(Clone, Copy, Debug)]
struct Tick {
  at: Instant,
}

/// A stretch the process did not run, in Unix milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Stall {
  started_at_ms: f64,
  done_at_ms: f64,
}

/// Starts the watch's thread, once per process. It sleeps until the app is in the foreground.
pub(crate) fn start() {
  STARTED.get_or_init(|| {
    let spawned = std::thread::Builder::new()
      .name("teamcraft-stall-watch".into())
      .spawn(watch);
    if let Err(error) = spawned {
      // The reports then name the job that held a lane, as before.
      tracing::warn!(%error, "could not start the stall watch");
    }
  });
}

/// The app came to the foreground: the watch runs.
pub(crate) fn foregrounded() {
  WATCH.lock().is_foreground = true;
  WATCH.foreground.notify_all();
}

/// The app went to the background: the watch stops until it is back.
pub(crate) fn backgrounded() {
  let mut state = WATCH.lock();
  state.is_foreground = false;
  state.stalls.pause();
}

/// How many milliseconds of `started_at_ms` to `done_at_ms` the process stalled, a stall that is
/// still under way included (the watch's thread wakes from it only after the job did).
pub(crate) fn stalled_ms(started_at_ms: f64, done_at_ms: f64) -> f64 {
  WATCH
    .lock()
    .stalls
    .stalled_ms(started_at_ms, done_at_ms, Instant::now(), perf_jobs::now_ms())
}

/// A stall as the watch would keep it, for tests of what is reported.
#[cfg(test)]
pub(crate) fn keep_stall(started_at_ms: f64, done_at_ms: f64) {
  WATCH.lock().stalls.done.push_back(Stall {
    started_at_ms,
    done_at_ms,
  });
}

/// The thread: a tick every [`TICK`] while the app is in the foreground.
fn watch() {
  loop {
    {
      let mut state = WATCH.lock();
      while !state.is_foreground {
        state = WATCH
          .foreground
          .wait(state)
          .unwrap_or_else(PoisonError::into_inner);
      }
    }
    std::thread::sleep(TICK);
    let mut state = WATCH.lock();
    if !state.is_foreground {
      continue;
    }
    if let Some(stall) = state.stalls.tick(Instant::now(), perf_jobs::now_ms()) {
      drop(state);
      tracing::info!(
        target: logging::LIFECYCLE,
        ms = (stall.done_at_ms - stall.started_at_ms).round() as u64,
        "the process did not run"
      );
    }
  }
}

impl Watch {
  const fn new() -> Self {
    Self {
      state: Mutex::new(WatchState {
        is_foreground: false,
        stalls: Stalls::new(),
      }),
      foreground: Condvar::new(),
    }
  }

  fn lock(&self) -> MutexGuard<'_, WatchState> {
    // Invariant-free data: at worst a stall is missed.
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl Stalls {
  const fn new() -> Self {
    Self {
      done: VecDeque::new(),
      maybe_last_tick: None,
    }
  }

  /// A tick at `at` (Unix `at_ms`): the stall it ends, if it came [`MIN_STALL`] or more late.
  fn tick(&mut self, at: Instant, at_ms: f64) -> Option<Stall> {
    let maybe_stall = self.stall_until(at, at_ms);
    if let Some(stall) = maybe_stall {
      if self.done.len() >= MAX_STALLS {
        self.done.pop_front();
      }
      self.done.push_back(stall);
    }
    self.maybe_last_tick = Some(Tick { at });
    maybe_stall
  }

  /// The watch stops (background): no tick is due until it runs again.
  fn pause(&mut self) {
    self.maybe_last_tick = None;
  }

  /// Milliseconds of `started_at_ms` to `done_at_ms` inside a stall, asked at `now` (`now_ms`).
  fn stalled_ms(&self, started_at_ms: f64, done_at_ms: f64, now: Instant, now_ms: f64) -> f64 {
    let maybe_open = self.stall_until(now, now_ms);
    self
      .done
      .iter()
      .chain(maybe_open.iter())
      .map(|stall| (stall.done_at_ms.min(done_at_ms) - stall.started_at_ms.max(started_at_ms)).max(0.0))
      .sum()
  }

  /// The stall from the tick that was due after the last one until `at`, if that is long enough.
  fn stall_until(&self, at: Instant, at_ms: f64) -> Option<Stall> {
    let last = self.maybe_last_tick?;
    let late = at.saturating_duration_since(last.at).checked_sub(TICK)?;
    (late >= MIN_STALL).then_some(Stall {
      started_at_ms: at_ms - late.as_secs_f64() * 1_000.0,
      done_at_ms: at_ms,
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const BASE_MS: f64 = 1.0e12;

  #[test]
  fn a_tick_on_time_is_no_stall() {
    let start = Instant::now();
    let mut stalls = Stalls::new();
    assert_eq!(stalls.tick(start, BASE_MS), None);
    let next = start + TICK + Duration::from_millis(300);
    assert_eq!(stalls.tick(next, BASE_MS + 800.0), None);
    assert_eq!(
      stalls.stalled_ms(BASE_MS, BASE_MS + 800.0, next, BASE_MS + 800.0),
      0.0
    );
  }

  /// The 1.0.41 case: nothing of the process ran for 7.2 s; every job inside that stretch is
  /// covered, also before the watch's own thread woke from it.
  #[test]
  fn a_late_tick_is_a_stall_from_when_it_was_due() {
    let start = Instant::now();
    let mut stalls = Stalls::new();
    stalls.tick(start, BASE_MS);
    let woke = start + TICK + Duration::from_millis(7_200);
    let woke_ms = BASE_MS + 7_700.0;

    // A job that ended in the same millisecond asks before the watch woke.
    let open = stalls.stalled_ms(BASE_MS + 200.0, woke_ms, woke, woke_ms);
    assert!((open - 7_200.0).abs() < 1.0, "{open}");

    let stall = stalls.tick(woke, woke_ms).expect("a stall");
    assert!((stall.started_at_ms - (BASE_MS + 500.0)).abs() < 1.0);
    assert_eq!(stall.done_at_ms, woke_ms);
    // Asked later, from what was kept; only the part inside the job's stretch counts.
    let later = woke + TICK;
    let kept = stalls.stalled_ms(BASE_MS + 4_500.0, woke_ms + 100.0, later, woke_ms + 500.0);
    assert!((kept - 3_200.0).abs() < 1.0, "{kept}");
  }

  #[test]
  fn the_background_is_no_stall() {
    let start = Instant::now();
    let mut stalls = Stalls::new();
    stalls.tick(start, BASE_MS);
    stalls.pause();
    // Back after a minute: the first tick starts afresh.
    let back = start + Duration::from_secs(60);
    assert_eq!(stalls.tick(back, BASE_MS + 60_000.0), None);
    assert_eq!(
      stalls.stalled_ms(BASE_MS, BASE_MS + 60_000.0, back, BASE_MS + 60_000.0),
      0.0
    );
  }

  #[test]
  fn kept_stalls_are_bounded() {
    let mut at = Instant::now();
    let mut at_ms = BASE_MS;
    let mut stalls = Stalls::new();
    stalls.tick(at, at_ms);
    for _ in 0..(MAX_STALLS + 10) {
      at += TICK + MIN_STALL;
      at_ms += 1_500.0;
      stalls.tick(at, at_ms);
    }
    assert_eq!(stalls.done.len(), MAX_STALLS);
  }
}
