//! Per-source rate limits: a token bucket that refills continuously.

use std::time::{Duration, Instant};

/// Allows `rate_per_minute` events per minute on average, with bursts up to the same number.
#[derive(Debug, Clone)]
pub struct RateLimiter {
  capacity: f64,
  tokens: f64,
  refill_per_second: f64,
  last_refill: Instant,
}

impl RateLimiter {
  pub fn new(rate_per_minute: u32, now: Instant) -> Self {
    let capacity = f64::from(rate_per_minute.max(1));
    Self {
      capacity,
      tokens: capacity,
      refill_per_second: capacity / 60.0,
      last_refill: now,
    }
  }

  /// Takes `count` events if there are enough tokens; otherwise takes none and returns `false`.
  pub fn try_take(&mut self, count: u32, now: Instant) -> bool {
    self.refill(now);
    let wanted = f64::from(count);
    if self.tokens >= wanted {
      self.tokens -= wanted;
      true
    } else {
      false
    }
  }

  /// Takes as many of `count` events as there are tokens for and returns how many were allowed.
  pub fn take_up_to(&mut self, count: u32, now: Instant) -> u32 {
    self.refill(now);
    // Truncation is intended: only whole events are allowed.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let allowed = (self.tokens.floor() as u32).min(count);
    self.tokens -= f64::from(allowed);
    allowed
  }

  /// How long until one event is allowed again.
  pub fn retry_after(&self) -> Duration {
    let missing = (1.0 - self.tokens).max(0.0);
    Duration::from_secs_f64(missing / self.refill_per_second).max(Duration::from_secs(1))
  }

  fn refill(&mut self, now: Instant) {
    let elapsed = now.saturating_duration_since(self.last_refill).as_secs_f64();
    self.tokens = (self.tokens + elapsed * self.refill_per_second).min(self.capacity);
    self.last_refill = now;
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_burst_up_to_the_rate_is_allowed_then_events_are_refused_until_the_bucket_refills() {
    let start = Instant::now();
    let mut limiter = RateLimiter::new(60, start);
    assert!(limiter.try_take(60, start));
    assert!(!limiter.try_take(1, start));
    // One token per second.
    assert!(limiter.try_take(1, start + Duration::from_millis(1100)));
    assert!(!limiter.try_take(2, start + Duration::from_millis(1200)));
  }

  #[test]
  fn the_bucket_never_holds_more_than_its_capacity() {
    let start = Instant::now();
    let mut limiter = RateLimiter::new(10, start);
    assert!(!limiter.try_take(11, start + Duration::from_secs(3600)));
    assert_eq!(limiter.take_up_to(100, start + Duration::from_secs(3600)), 10);
  }

  #[test]
  fn take_up_to_hands_out_what_is_left() {
    let start = Instant::now();
    let mut limiter = RateLimiter::new(5, start);
    assert_eq!(limiter.take_up_to(3, start), 3);
    assert_eq!(limiter.take_up_to(3, start), 2);
    assert_eq!(limiter.take_up_to(3, start), 0);
  }

  #[test]
  fn retry_after_is_at_least_a_second() {
    let start = Instant::now();
    let mut limiter = RateLimiter::new(600, start);
    limiter.take_up_to(600, start);
    assert!(limiter.retry_after() >= Duration::from_secs(1));
  }
}
