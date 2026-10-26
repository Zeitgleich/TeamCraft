//! The retry policy of every retry loop in TeamCraft (issue #743): capped exponential backoff with
//! "equal jitter", and a server's `Retry-After` as the lower bound of the wait.
//!
//! Pure arithmetic without clocks, sleeps or a random number generator of its own, so it runs in
//! the browser, on the server and in tests alike: callers pass the failure count and a [`Jitter`]
//! (seeded in tests), and wait with their own clock.

use std::time::Duration;

/// The default first wait.
pub const DEFAULT_INITIAL: Duration = Duration::from_secs(1);
/// The default longest wait.
pub const DEFAULT_MAX: Duration = Duration::from_secs(30);
/// Doublings beyond this don't change anything any more (2^32 times the first wait is longer than
/// any sensible cap) and would overflow.
const MAX_DOUBLINGS: u32 = 32;

/// How long to wait before the next attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
  /// The wait after the first failure.
  pub initial: Duration,
  /// The wait doubles with every failure up to this. A `Retry-After` longer than this is cut to it
  /// too, so a confused server can't park a client for a day.
  pub max: Duration,
  pub jitter: JitterMode,
}

/// How the wait is spread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JitterMode {
  /// Half the backoff plus a random part of the other half: clients that lost the server at the
  /// same moment (a restart, an outage) don't come back in lock-step, and the wait never drops
  /// below half the backoff.
  #[default]
  Equal,
  /// Exactly the backoff (tests that step a clock by the backoff).
  None,
}

/// A small xorshift generator: the backoff only needs spread, not secrecy, and tests seed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Jitter(u64);

impl RetryPolicy {
  pub const fn new(initial: Duration, max: Duration) -> Self {
    Self {
      initial,
      max,
      jitter: JitterMode::Equal,
    }
  }

  /// The same policy with another jitter mode.
  pub const fn with_jitter(mut self, jitter: JitterMode) -> Self {
    self.jitter = jitter;
    self
  }

  /// The wait before the next attempt after `failures` failed ones in a row (0: the first retry),
  /// at least `maybe_retry_after` (the server's `Retry-After`) and at most [`Self::max`].
  pub fn delay(&self, failures: u32, jitter: &mut Jitter, maybe_retry_after: Option<Duration>) -> Duration {
    let spread = self.jittered(failures, jitter.fraction());
    match maybe_retry_after {
      Some(retry_after) => spread.max(retry_after.min(self.cap())),
      None => spread,
    }
  }

  /// The backoff after `failures` failures spread by `fraction` in `[0, 1]` (clamped): for callers
  /// with their own random source or their own rule for `Retry-After` (a push service's is binding
  /// beyond the cap).
  pub fn jittered(&self, failures: u32, fraction: f64) -> Duration {
    let backoff = self.backoff(failures);
    match self.jitter {
      JitterMode::Equal => {
        let half = backoff / 2;
        half + (backoff - half).mul_f64(fraction.clamp(0.0, 1.0))
      },
      JitterMode::None => backoff,
    }
  }

  /// The backoff without jitter: `initial` doubled `failures` times, at most `max`.
  pub fn backoff(&self, failures: u32) -> Duration {
    let factor = 1_u64 << failures.min(MAX_DOUBLINGS);
    let factor = u32::try_from(factor).unwrap_or(u32::MAX);
    self.initial.saturating_mul(factor).min(self.cap())
  }

  /// `max`, but never below `initial` (a misconfigured cap doesn't shorten the first wait).
  fn cap(&self) -> Duration {
    self.max.max(self.initial)
  }
}

impl Default for RetryPolicy {
  fn default() -> Self {
    Self::new(DEFAULT_INITIAL, DEFAULT_MAX)
  }
}

impl Jitter {
  pub fn new(seed: u64) -> Self {
    // Xorshift never leaves zero.
    Self(seed.max(1))
  }

  /// A number in `0..bound` (0 for an empty range).
  pub fn below(&mut self, bound: u64) -> u64 {
    let x = self.next();
    if bound == 0 { 0 } else { x % bound }
  }

  /// A fraction in `[0, 1)`.
  pub fn fraction(&mut self) -> f64 {
    // The top 53 bits fill an f64 mantissa exactly.
    (self.next() >> 11) as f64 / (1_u64 << 53) as f64
  }

  fn next(&mut self) -> u64 {
    let mut x = self.0;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    self.0 = x;
    x
  }
}

/// `Retry-After` as delay-seconds (RFC 9110 §10.2.3). The HTTP-date form is not used by our server
/// and depends on the local clock, so it is ignored.
pub fn parse_retry_after(value: &str) -> Option<Duration> {
  value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
  use super::*;

  const POLICY: RetryPolicy = RetryPolicy::new(Duration::from_secs(1), Duration::from_secs(30));

  #[test]
  fn the_backoff_doubles_up_to_the_cap() {
    let waits: Vec<u64> = (0..8)
      .map(|failures| POLICY.backoff(failures).as_secs())
      .collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
    assert_eq!(POLICY.backoff(u32::MAX), Duration::from_secs(30));
  }

  #[test]
  fn equal_jitter_stays_between_half_and_all_of_the_backoff_and_varies() {
    let mut jitter = Jitter::new(7);
    let waits: Vec<Duration> = (0..200).map(|_| POLICY.delay(3, &mut jitter, None)).collect();
    assert!(
      waits
        .iter()
        .all(|wait| (Duration::from_secs(4)..=Duration::from_secs(8)).contains(wait))
    );
    assert!(waits.iter().any(|wait| *wait != waits[0]));
  }

  #[test]
  fn the_fraction_spreads_the_upper_half() {
    assert_eq!(POLICY.jittered(3, 0.0), Duration::from_secs(4));
    assert_eq!(POLICY.jittered(3, 0.5), Duration::from_secs(6));
    assert_eq!(POLICY.jittered(3, 7.0), Duration::from_secs(8), "clamped");
  }

  #[test]
  fn without_jitter_the_wait_is_the_backoff() {
    let policy = POLICY.with_jitter(JitterMode::None);
    let mut jitter = Jitter::new(1);
    assert_eq!(policy.delay(2, &mut jitter, None), Duration::from_secs(4));
  }

  #[test]
  fn retry_after_is_a_lower_bound_cut_to_the_cap() {
    let policy = POLICY.with_jitter(JitterMode::None);
    let wait = |failures, retry_after| {
      policy.delay(
        failures,
        &mut Jitter::new(1),
        Some(Duration::from_secs(retry_after)),
      )
    };
    assert_eq!(wait(0, 10), Duration::from_secs(10));
    // Shorter than the backoff: the backoff wins.
    assert_eq!(wait(3, 2), Duration::from_secs(8));
    // A day: cut to the cap.
    assert_eq!(wait(0, 86_400), Duration::from_secs(30));
  }

  #[test]
  fn a_cap_below_the_first_wait_does_not_shorten_it() {
    let policy =
      RetryPolicy::new(Duration::from_secs(5), Duration::from_secs(1)).with_jitter(JitterMode::None);
    assert_eq!(policy.delay(0, &mut Jitter::new(1), None), Duration::from_secs(5));
  }

  #[test]
  fn jitter_draws_stay_in_range() {
    let mut jitter = Jitter::new(0);
    assert!((0..1_000).all(|_| jitter.below(10) < 10));
    assert_eq!(jitter.below(0), 0);
    assert!((0..1_000).all(|_| (0.0..1.0).contains(&jitter.fraction())));
    assert_ne!(Jitter::new(1).fraction(), Jitter::new(2).fraction());
  }

  #[test]
  fn retry_after_parses_delay_seconds_only() {
    assert_eq!(parse_retry_after(" 30 "), Some(Duration::from_secs(30)));
    assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    assert_eq!(parse_retry_after("-1"), None);
    assert_eq!(parse_retry_after(""), None);
  }
}
