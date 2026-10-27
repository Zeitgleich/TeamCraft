//! Rate limits: token buckets that refill continuously, for the requests and lines of a source and
//! of all sources together, and for the bytes of webhooks with files.

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
    Self::per_minute(u64::from(rate_per_minute), now)
  }

  /// A bucket for amounts rather than events, e.g. bytes: `amount_per_minute` a minute on average,
  /// in bursts up to the same amount.
  pub fn per_minute(amount_per_minute: u64, now: Instant) -> Self {
    // Exact up to 2^53, far above any configured limit.
    let capacity = amount_per_minute.max(1) as f64;
    Self {
      capacity,
      tokens: capacity,
      refill_per_second: capacity / 60.0,
      last_refill: now,
    }
  }

  /// Takes `count` events if there are enough tokens; otherwise takes none and returns `false`.
  pub fn try_take(&mut self, count: u32, now: Instant) -> bool {
    self.try_take_amount(u64::from(count), now)
  }

  /// [`Self::try_take`] for an amount (bytes): all of it or nothing.
  pub fn try_take_amount(&mut self, amount: u64, now: Instant) -> bool {
    self.refill(now);
    let wanted = amount as f64;
    if self.tokens >= wanted {
      self.tokens -= wanted;
      true
    } else {
      false
    }
  }

  /// Whether `amount` fits into the bucket at all; a bigger one is never allowed.
  pub fn can_ever_take(&self, amount: u64) -> bool {
    amount as f64 <= self.capacity
  }

  /// Gives back what was taken for work that didn't happen (a request refused half way).
  pub fn refund(&mut self, amount: u64, now: Instant) {
    self.refill(now);
    self.tokens = (self.tokens + amount as f64).min(self.capacity);
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
    self.retry_after_for(1)
  }

  /// How long until `amount` is allowed again (at least a second; an amount above the capacity
  /// waits for a full bucket).
  pub fn retry_after_for(&self, amount: u64) -> Duration {
    let missing = ((amount as f64).min(self.capacity) - self.tokens).max(0.0);
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
  fn amounts_are_taken_whole_and_say_how_long_until_they_fit() {
    let start = Instant::now();
    let mut limiter = RateLimiter::per_minute(6_000_000, start);
    assert!(limiter.try_take_amount(5_000_000, start));
    assert!(!limiter.try_take_amount(2_000_000, start), "only 1 MB is left");
    // 100 kB a second refill: the missing million takes ten seconds.
    assert_eq!(limiter.retry_after_for(2_000_000), Duration::from_secs(10));
    assert!(limiter.try_take_amount(2_000_000, start + Duration::from_secs(10)));
    assert!(limiter.can_ever_take(6_000_000));
    assert!(!limiter.can_ever_take(6_000_001));
  }

  #[test]
  fn a_refund_gives_back_but_never_beyond_the_capacity() {
    let start = Instant::now();
    let mut limiter = RateLimiter::per_minute(1000, start);
    assert!(limiter.try_take_amount(800, start));
    limiter.refund(800, start);
    assert!(limiter.try_take_amount(1000, start));
    limiter.refund(5000, start);
    assert!(!limiter.try_take_amount(1001, start));
  }

  #[test]
  fn retry_after_is_at_least_a_second() {
    let start = Instant::now();
    let mut limiter = RateLimiter::new(600, start);
    limiter.take_up_to(600, start);
    assert!(limiter.retry_after() >= Duration::from_secs(1));
  }
}
