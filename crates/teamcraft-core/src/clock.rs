//! Wall-clock time, injectable so tests are deterministic (testing.md) and the browser, where
//! `std::time::SystemTime` isn't available, can supply its own.

use std::sync::atomic::{AtomicI64, Ordering};

pub trait Clock {
  /// Milliseconds since the Unix epoch.
  fn now_ms(&self) -> i64;
}

/// The operating system clock.
#[cfg(not(target_arch = "wasm32"))]
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

impl Clock for ManualClock {
  fn now_ms(&self) -> i64 {
    self.now_ms.load(Ordering::Relaxed)
  }
}
