//! A token bucket for the state a participant reports about itself (mute): an honest client sends
//! a few changes, a hostile member must not flood the room with toggles that every other
//! participant has to be told about.

use std::time::Instant;

/// Allows `burst` events at once and `per_second` more every second.
#[derive(Debug, Clone)]
pub struct TokenBucket {
  burst: f64,
  per_second: f64,
  tokens: f64,
  last: Instant,
}

impl TokenBucket {
  /// A full bucket as of `now`.
  pub fn new(burst: u32, per_second: u32, now: Instant) -> Self {
    let burst = f64::from(burst.max(1));
    Self {
      burst,
      per_second: f64::from(per_second),
      tokens: burst,
      last: now,
    }
  }

  /// Takes a token if there is one.
  pub fn allow(&mut self, now: Instant) -> bool {
    let elapsed = now.saturating_duration_since(self.last);
    self.last = now.max(self.last);
    self.tokens = (self.tokens + elapsed.as_secs_f64() * self.per_second).min(self.burst);
    if self.tokens < 1.0 {
      return false;
    }
    self.tokens -= 1.0;
    true
  }
}

#[cfg(test)]
mod tests {
  use std::time::Duration;

  use super::*;

  #[test]
  fn a_burst_passes_then_the_rate_applies() {
    let start = Instant::now();
    let mut bucket = TokenBucket::new(3, 1, start);
    assert!((0..3).all(|_| bucket.allow(start)));
    assert!(!bucket.allow(start));
    assert!(!bucket.allow(start + Duration::from_millis(900)));
    assert!(bucket.allow(start + Duration::from_millis(1000)));
    assert!(!bucket.allow(start + Duration::from_millis(1100)));
  }

  #[test]
  fn tokens_do_not_pile_up_beyond_the_burst() {
    let start = Instant::now();
    let mut bucket = TokenBucket::new(2, 10, start);
    let later = start + Duration::from_secs(60);
    assert!(bucket.allow(later));
    assert!(bucket.allow(later));
    assert!(!bucket.allow(later));
  }
}
