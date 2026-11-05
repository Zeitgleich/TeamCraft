//! How often one device may join one call (#1227), on either transport: the one place joins are
//! paced, in [`crate::server::admit`].
//!
//! Every join of a device makes the call's DAVE group re-key twice: once without the device when its
//! last connection went (e2ee.md §8: a lost device must not hold a current media key), once with it
//! when its new key package arrives. A member with a valid join token who loops connect, join, drop
//! would make every other participant re-key with each cycle, and leave a session behind that waits
//! through the reconnect grace. So each (call, device) gets a token bucket: a burst of joins at once,
//! then one more per interval. A join over it waits for its turn when that is near (it is delayed,
//! not ended: a client that doesn't know the rule just sees a slower join), and is refused with
//! `rate_limited` and the seconds to wait otherwise. Clients take that refusal as a failed rejoin
//! attempt and try again after it, without ending the call.
//!
//! Honest clients never get near it: they pace their own rejoins when the network keeps dropping
//! (`teamcraft_media::call_resilience::RejoinPacer`), slower than the bucket refills.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use teamcraft_proto::{CallId, DeviceId};

/// The pace of one device's joins of one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejoinLimit {
  /// Joins at once: the first one and a few quick reconnects.
  pub burst: u32,
  /// One more join per this long, beyond the burst.
  pub interval: Duration,
  /// A join whose turn comes within this long waits for it; a later one is refused. Below the
  /// clients' stall timeout (5 s), so a waiting join doesn't look like a dead path to them.
  pub max_delay: Duration,
}

impl Default for RejoinLimit {
  fn default() -> Self {
    Self {
      burst: 5,
      interval: Duration::from_secs(10),
      max_delay: Duration::from_secs(2),
    }
  }
}

/// What a join may do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejoinTurn {
  /// Join at once.
  Now,
  /// Join after waiting this long (its turn is taken already).
  After(Duration),
  /// Too soon: try again after this long.
  Refused { retry_after: Duration },
}

/// The buckets of the devices that joined a call lately.
#[derive(Debug)]
pub struct RejoinLimiter {
  limit: RejoinLimit,
  state: Mutex<State>,
}

#[derive(Debug)]
struct State {
  budgets: HashMap<(CallId, DeviceId), Budget>,
  maybe_swept_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
struct Budget {
  /// Joins left; below zero while a delayed join waits for its turn.
  tokens: f64,
  at: Instant,
}

impl RejoinLimiter {
  pub fn new(limit: RejoinLimit) -> Self {
    Self {
      limit,
      state: Mutex::new(State {
        budgets: HashMap::new(),
        maybe_swept_at: None,
      }),
    }
  }

  /// Takes the turn of a join of `device_id` into `call_id` at `now`, unless it is refused.
  pub fn take_turn(&self, call_id: CallId, device_id: DeviceId, now: Instant) -> RejoinTurn {
    let burst = f64::from(self.limit.burst.max(1));
    let interval = self.limit.interval.as_secs_f64();
    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
    state.sweep(now, burst, interval, self.limit.interval);
    let budget = state.budgets.entry((call_id, device_id)).or_insert(Budget {
      tokens: burst,
      at: now,
    });
    budget.refill(now, burst, interval);
    if budget.tokens >= 1.0 {
      budget.tokens -= 1.0;
      return RejoinTurn::Now;
    }
    let wait = if interval > 0.0 {
      Duration::from_secs_f64((1.0 - budget.tokens) * interval)
    } else {
      Duration::ZERO
    };
    if wait <= self.limit.max_delay {
      budget.tokens -= 1.0;
      RejoinTurn::After(wait)
    } else {
      RejoinTurn::Refused { retry_after: wait }
    }
  }

  #[cfg(test)]
  fn tracked(&self) -> usize {
    self
      .state
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .budgets
      .len()
  }
}

impl State {
  /// Forgets the buckets that are full again: those devices may join as if new. At most once per
  /// interval, so the map holds no more than the devices that joined within the last few minutes.
  fn sweep(&mut self, now: Instant, burst: f64, interval: f64, every: Duration) {
    if self
      .maybe_swept_at
      .is_some_and(|swept_at| now.saturating_duration_since(swept_at) < every)
    {
      return;
    }
    self.maybe_swept_at = Some(now);
    self.budgets.retain(|_, budget| {
      let mut budget = *budget;
      budget.refill(now, burst, interval);
      budget.tokens < burst
    });
  }
}

impl Budget {
  fn refill(&mut self, now: Instant, burst: f64, interval: f64) {
    let elapsed = now.saturating_duration_since(self.at).as_secs_f64();
    self.at = now.max(self.at);
    self.tokens = if interval > 0.0 {
      (self.tokens + elapsed / interval).min(burst)
    } else {
      burst
    };
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const LIMIT: RejoinLimit = RejoinLimit {
    burst: 3,
    interval: Duration::from_secs(10),
    max_delay: Duration::from_secs(2),
  };

  fn call() -> CallId {
    id(1)
  }

  fn device(raw: i64) -> DeviceId {
    id(raw)
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  #[test]
  fn a_burst_joins_at_once_then_a_near_turn_waits_and_a_far_one_is_refused() {
    let limiter = RejoinLimiter::new(LIMIT);
    let start = Instant::now();
    for _ in 0..3 {
      assert_eq!(limiter.take_turn(call(), device(1), start), RejoinTurn::Now);
    }
    // 9 s later 0.9 of a join is back: the next turn is 1 s away, near enough to wait for.
    let later = start + Duration::from_secs(9);
    assert_eq!(
      limiter.take_turn(call(), device(1), later),
      RejoinTurn::After(Duration::from_secs(1))
    );
    // That turn is taken: the one after is 11 s away.
    let RejoinTurn::Refused { retry_after } = limiter.take_turn(call(), device(1), later) else {
      panic!("expected a refusal");
    };
    assert!((retry_after.as_secs_f64() - 11.0).abs() < 0.01, "{retry_after:?}");
    // Refusals cost nothing: after waiting as told, the join goes through at once.
    assert_eq!(
      limiter.take_turn(call(), device(1), later + retry_after),
      RejoinTurn::Now
    );
  }

  #[test]
  fn a_looping_device_joins_once_per_interval_however_often_it_tries() {
    let limiter = RejoinLimiter::new(LIMIT);
    let start = Instant::now();
    let mut joins = 0;
    let mut delays = Duration::ZERO;
    // A join every 100 ms for ten minutes.
    for step in 0..6000u64 {
      match limiter.take_turn(call(), device(1), start + Duration::from_millis(step * 100)) {
        RejoinTurn::Now => joins += 1,
        RejoinTurn::After(wait) => {
          joins += 1;
          delays += wait;
        },
        RejoinTurn::Refused { .. } => {},
      }
    }
    // The burst, then one per 10 s: 3 + 60, not 6000.
    assert!((60..=64).contains(&joins), "{joins}");
    assert!(delays <= LIMIT.max_delay * joins);
  }

  #[test]
  fn devices_and_calls_have_their_own_turns() {
    let limiter = RejoinLimiter::new(LIMIT);
    let now = Instant::now();
    for _ in 0..3 {
      limiter.take_turn(call(), device(1), now);
    }
    assert!(matches!(
      limiter.take_turn(call(), device(1), now),
      RejoinTurn::Refused { .. }
    ));
    assert_eq!(limiter.take_turn(call(), device(2), now), RejoinTurn::Now);
    assert_eq!(limiter.take_turn(id(2), device(1), now), RejoinTurn::Now);
  }

  #[test]
  fn an_occasional_reconnect_is_never_held_up() {
    let limiter = RejoinLimiter::new(LIMIT);
    let start = Instant::now();
    // A join, and a reconnect every 30 s for an hour.
    for step in 0..120u64 {
      assert_eq!(
        limiter.take_turn(call(), device(1), start + Duration::from_secs(step * 30)),
        RejoinTurn::Now
      );
    }
  }

  #[test]
  fn full_buckets_are_forgotten() {
    let limiter = RejoinLimiter::new(LIMIT);
    let start = Instant::now();
    for raw in 1..=100 {
      limiter.take_turn(call(), device(raw), start);
    }
    assert_eq!(limiter.tracked(), 100);
    // Refilled after 10 s; the next join sweeps them.
    limiter.take_turn(call(), device(1000), start + Duration::from_secs(11));
    assert_eq!(limiter.tracked(), 1);
  }
}
