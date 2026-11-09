//! Wall-clock time, injectable so tests are deterministic (testing.md) and the browser, where
//! `std::time::SystemTime` isn't available, can supply its own.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

use crate::retry::millis;

pub trait Clock {
  /// Milliseconds since the Unix epoch.
  fn now_ms(&self) -> i64;
}

/// A wall-clock deadline that lies further ahead than `longest` (the longest wait the loop ever
/// sets) can only have been set before the clock went back: it counts from now instead, so the
/// work runs now and not after the jump (robustness.md, "Time").
pub fn rewound(deadline_ms: i64, now_ms: i64, longest: Duration) -> i64 {
  if deadline_ms.saturating_sub(now_ms) > millis(longest) {
    now_ms
  } else {
    deadline_ms
  }
}

/// How long a loop sleeps until `deadline_ms`: never longer than `longest`, so a deadline the
/// clock's jump back pushed into the "future" is looked at again within one `longest`.
pub fn sleep_ms_until(deadline_ms: i64, now_ms: i64, longest: Duration) -> i64 {
  deadline_ms.saturating_sub(now_ms).clamp(0, millis(longest))
}

/// The operating system clock (`Date.now()` in the browser).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

/// A clock that only moves when told to.
#[derive(Debug, Default)]
pub struct ManualClock {
  now_ms: AtomicI64,
}

#[cfg(not(target_arch = "wasm32"))]
impl Clock for SystemClock {
  fn now_ms(&self) -> i64 {
    let since_epoch = std::time::SystemTime::now()
      .duration_since(std::time::UNIX_EPOCH)
      .unwrap_or_default();
    i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX)
  }
}

#[cfg(target_arch = "wasm32")]
impl Clock for SystemClock {
  fn now_ms(&self) -> i64 {
    // Whole milliseconds since the epoch fit an f64 exactly.
    js_sys::Date::now() as i64
  }
}

impl ManualClock {
  pub fn new(now_ms: i64) -> Self {
    Self {
      now_ms: AtomicI64::new(now_ms),
    }
  }

  pub fn advance(&self, milliseconds: i64) {
    self.now_ms.fetch_add(milliseconds, Ordering::Relaxed);
  }
}

/// One clock shared by several clients (tests that move time for all of them).
impl<C: Clock + ?Sized> Clock for std::sync::Arc<C> {
  fn now_ms(&self) -> i64 {
    (**self).now_ms()
  }
}

impl Clock for ManualClock {
  fn now_ms(&self) -> i64 {
    self.now_ms.load(Ordering::Relaxed)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const TICK: Duration = Duration::from_secs(3);
  const HOUR_MS: i64 = 3_600_000;

  #[test]
  fn a_deadline_within_reach_stays() {
    assert_eq!(rewound(10_000 + 2_000, 10_000, TICK), 12_000);
    assert_eq!(rewound(10_000 + 3_000, 10_000, TICK), 13_000);
    assert_eq!(rewound(5_000, 10_000, TICK), 5_000, "overdue stays overdue");
  }

  #[test]
  fn a_deadline_from_before_a_jump_back_counts_from_now() {
    let before_jump = 10 * HOUR_MS;
    let deadline = before_jump + 3_000;
    let now = before_jump - HOUR_MS;
    assert_eq!(rewound(deadline, now, TICK), now);
  }

  #[test]
  fn the_sleep_is_never_longer_than_the_longest_wait() {
    assert_eq!(sleep_ms_until(10_000 + HOUR_MS, 10_000, TICK), 3_000);
    assert_eq!(sleep_ms_until(11_000, 10_000, TICK), 1_000);
    assert_eq!(sleep_ms_until(9_000, 10_000, TICK), 0);
  }
}
