use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use teamcraft_proto::{Id, SNOWFLAKE_EPOCH_MS, SNOWFLAKE_SEQUENCE_BITS, SNOWFLAKE_WORKER_BITS, Snowflake};

const MAX_WORKER: u16 = (1 << SNOWFLAKE_WORKER_BITS) - 1;
const MAX_SEQUENCE: u16 = (1 << SNOWFLAKE_SEQUENCE_BITS) - 1;
/// 41 bits of milliseconds since the custom epoch (about 69 years).
const MAX_TIMESTAMP: i64 = (1 << 41) - 1;

/// Source of the current time in Unix milliseconds; injectable for deterministic tests.
pub trait Clock: Send + Sync {
  fn now_unix_millis(&self) -> i64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
  fn now_unix_millis(&self) -> i64 {
    SystemTime::now()
      .duration_since(UNIX_EPOCH)
      .ok()
      .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
      .unwrap_or(0)
  }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnowflakeError {
  #[error("worker ID {0} exceeds the maximum of {MAX_WORKER}")]
  WorkerOutOfRange(u16),
  #[error("clock is before the Snowflake epoch or beyond the 41-bit range")]
  ClockOutOfRange,
}

/// Generates unique, time-sortable IDs for one worker.
///
/// IDs are strictly increasing per generator. A clock that stands still or moves backwards never
/// repeats an ID: the generator keeps using its last millisecond and advances it logically when
/// the 4096 sequence numbers of a millisecond are used up. It never sleeps.
pub struct SnowflakeGenerator<C: Clock = SystemClock> {
  worker: u16,
  clock: C,
  state: Mutex<State>,
}

#[derive(Debug, Clone, Copy, Default)]
struct State {
  last_millis: i64,
  sequence: u16,
}

impl SnowflakeGenerator<SystemClock> {
  pub fn new(worker: u16) -> Result<Self, SnowflakeError> {
    Self::with_clock(worker, SystemClock)
  }
}

impl<C: Clock> SnowflakeGenerator<C> {
  pub fn with_clock(worker: u16, clock: C) -> Result<Self, SnowflakeError> {
    if worker > MAX_WORKER {
      return Err(SnowflakeError::WorkerOutOfRange(worker));
    }
    Ok(Self {
      worker,
      clock,
      state: Mutex::new(State::default()),
    })
  }

  /// The next typed ID.
  pub fn next_id<T: Id>(&self) -> Result<T, SnowflakeError> {
    self.next_snowflake().map(T::from_snowflake)
  }

  pub fn next_snowflake(&self) -> Result<Snowflake, SnowflakeError> {
    let now = self.clock.now_unix_millis() - SNOWFLAKE_EPOCH_MS;
    if now < 0 {
      return Err(SnowflakeError::ClockOutOfRange);
    }
    // A poisoned lock only means another thread panicked mid-update of two plain integers; the state
    // is still a valid (monotonic) value, so keep going.
    let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut millis = now.max(state.last_millis);
    if millis == state.last_millis {
      if state.sequence == MAX_SEQUENCE {
        millis += 1;
        state.sequence = 0;
      } else {
        state.sequence += 1;
      }
    } else {
      state.sequence = 0;
    }
    if !(0..=MAX_TIMESTAMP).contains(&millis) {
      return Err(SnowflakeError::ClockOutOfRange);
    }
    state.last_millis = millis;

    let raw = (millis << (SNOWFLAKE_SEQUENCE_BITS + SNOWFLAKE_WORKER_BITS))
      | (i64::from(self.worker) << SNOWFLAKE_SEQUENCE_BITS)
      | i64::from(state.sequence);
    // Invariant: the timestamp is at most 41 bits, so the value is below 2^63 and non-negative.
    Snowflake::new(raw).map_err(|_| SnowflakeError::ClockOutOfRange)
  }
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicI64, Ordering};

  use teamcraft_proto::UserId;

  use super::*;

  const T0: i64 = SNOWFLAKE_EPOCH_MS + 10_000;

  #[test]
  fn encodes_time_worker_and_sequence() {
    let generator = SnowflakeGenerator::with_clock(7, FakeClock::at(T0)).unwrap();
    let first = generator.next_snowflake().unwrap();
    let second = generator.next_snowflake().unwrap();
    assert_eq!(first.unix_millis(), T0);
    assert_eq!(first.worker(), 7);
    assert_eq!((first.sequence(), second.sequence()), (0, 1));
  }

  #[test]
  fn ids_increase_when_time_advances() {
    let clock = FakeClock::at(T0);
    let generator = SnowflakeGenerator::with_clock(0, clock).unwrap();
    let first = generator.next_snowflake().unwrap();
    generator.clock.set(T0 + 1);
    let second = generator.next_snowflake().unwrap();
    assert!(second > first);
    assert_eq!(second.sequence(), 0);
  }

  #[test]
  fn sequence_overflow_advances_logical_millisecond() {
    let generator = SnowflakeGenerator::with_clock(0, FakeClock::at(T0)).unwrap();
    let mut last = generator.next_snowflake().unwrap();
    for _ in 0..=MAX_SEQUENCE {
      let next = generator.next_snowflake().unwrap();
      assert!(next > last);
      last = next;
    }
    assert_eq!(last.unix_millis(), T0 + 1);
  }

  #[test]
  fn clock_moving_backwards_never_repeats() {
    let generator = SnowflakeGenerator::with_clock(0, FakeClock::at(T0)).unwrap();
    let first = generator.next_snowflake().unwrap();
    generator.clock.set(T0 - 500);
    let second = generator.next_snowflake().unwrap();
    assert!(second > first);
  }

  #[test]
  fn rejects_invalid_worker_and_clock() {
    assert_eq!(
      SnowflakeGenerator::with_clock(MAX_WORKER + 1, FakeClock::at(T0)).err(),
      Some(SnowflakeError::WorkerOutOfRange(MAX_WORKER + 1))
    );
    let before_epoch = SnowflakeGenerator::with_clock(0, FakeClock::at(SNOWFLAKE_EPOCH_MS - 1)).unwrap();
    assert_eq!(
      before_epoch.next_snowflake(),
      Err(SnowflakeError::ClockOutOfRange)
    );
  }

  #[test]
  fn typed_ids_work() {
    let generator = SnowflakeGenerator::with_clock(1, FakeClock::at(T0)).unwrap();
    let id: UserId = generator.next_id().unwrap();
    assert_eq!(id.snowflake().worker(), 1);
  }

  struct FakeClock(AtomicI64);

  impl FakeClock {
    fn at(millis: i64) -> Self {
      Self(AtomicI64::new(millis))
    }

    fn set(&self, millis: i64) {
      self.0.store(millis, Ordering::SeqCst);
    }
  }

  impl Clock for FakeClock {
    fn now_unix_millis(&self) -> i64 {
      self.0.load(Ordering::SeqCst)
    }
  }
}
