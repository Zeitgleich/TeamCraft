//! In-memory limits: per-server token buckets and daily quotas, per-token windows, the replay
//! cache for request IDs, and the registration limits. Nothing here is persisted except the daily
//! counters, which the flush task writes to the store. Time is passed in (milliseconds), so tests
//! don't sleep.

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard};

use crate::store::UsageCounts;

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 3_600_000;
pub const DAY_MS: u64 = 86_400_000;
/// Entries kept at most; beyond this the sweep drops the oldest first. A guard against floods of
/// distinct tokens, not something a healthy server reaches.
const MAX_TRACKED_TOKENS: usize = 1_000_000;

/// Why a request is held back, with how long the sender should wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limited {
  ServerRate { retry_after_secs: u32 },
  DailyQuota { retry_after_secs: u32 },
  Token { retry_after_secs: u32 },
  Replay,
}

/// A server's effective limits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ServerLimits {
  pub pushes_per_second: u32,
  pub burst: u32,
  pub pushes_per_day: u64,
}

#[derive(Default)]
pub struct Limiter {
  buckets: Mutex<HashMap<String, Bucket>>,
  days: Mutex<HashMap<String, DayCounters>>,
  tokens: Mutex<HashMap<u64, Window>>,
  replay: Mutex<HashMap<(String, [u8; 16]), u64>>,
  registrations: Mutex<RegistrationWindows>,
  /// Keyed per process, so token fingerprints can't be precomputed and are gone on restart.
  token_hasher: RandomState,
}

struct Bucket {
  tokens: f64,
  updated_ms: u64,
}

struct Window {
  started_ms: u64,
  count: u32,
}

struct DayCounters {
  day: i64,
  /// Pushes counted for the day, including what the store already holds.
  total: u64,
  /// Not yet written to the store.
  pending: UsageCounts,
}

#[derive(Default)]
struct RegistrationWindows {
  per_address: HashMap<String, Window>,
  global: Option<Window>,
}

impl Limiter {
  /// Whether the server's count for `day` is known; if not, the caller seeds it from the store.
  pub fn is_day_loaded(&self, server_id: &str, day: i64) -> bool {
    lock(&self.days)
      .get(server_id)
      .is_some_and(|counters| counters.day == day)
  }

  /// Sets the count the store already holds for `day` (keeps what is not yet written).
  pub fn seed_day(&self, server_id: &str, day: i64, total: u64) {
    let mut days = lock(&self.days);
    let pending = days
      .get(server_id)
      .map(|counters| counters.pending)
      .unwrap_or_default();
    days.insert(server_id.to_owned(), DayCounters { day, total, pending });
  }

  /// Takes one push from the server's bucket and daily quota.
  pub fn admit_server(
    &self,
    server_id: &str,
    limits: ServerLimits,
    day: i64,
    now_ms: u64,
  ) -> Result<(), Limited> {
    {
      let mut days = lock(&self.days);
      let counters = days.entry(server_id.to_owned()).or_insert_with(|| DayCounters {
        day,
        total: 0,
        pending: UsageCounts::default(),
      });
      if counters.total >= limits.pushes_per_day {
        let into_day = now_ms % DAY_MS;
        return Err(Limited::DailyQuota {
          retry_after_secs: secs_ceil(DAY_MS - into_day),
        });
      }
    }
    let mut buckets = lock(&self.buckets);
    let bucket = buckets.entry(server_id.to_owned()).or_insert(Bucket {
      tokens: f64::from(limits.burst),
      updated_ms: now_ms,
    });
    let rate = f64::from(limits.pushes_per_second.max(1));
    let elapsed = now_ms.saturating_sub(bucket.updated_ms) as f64 / 1000.0;
    bucket.tokens = (bucket.tokens + elapsed * rate).min(f64::from(limits.burst.max(1)));
    bucket.updated_ms = now_ms;
    if bucket.tokens < 1.0 {
      let wait = ((1.0 - bucket.tokens) / rate).ceil().max(1.0);
      return Err(Limited::ServerRate {
        retry_after_secs: wait as u32,
      });
    }
    bucket.tokens -= 1.0;
    Ok(())
  }

  /// Counts a push towards the day once the push service has answered.
  pub fn record(&self, server_id: &str, outcome: Outcome) {
    let mut days = lock(&self.days);
    if let Some(counters) = days.get_mut(server_id) {
      counters.total = counters.total.saturating_add(1);
      match outcome {
        Outcome::Accepted => counters.pending.accepted += 1,
        Outcome::Gone => counters.pending.gone += 1,
        Outcome::Failed => counters.pending.failed += 1,
      }
    }
  }

  /// Takes the counters since the last flush, with the day they were counted on.
  pub fn drain_pending(&self) -> Vec<(String, i64, UsageCounts)> {
    let mut days = lock(&self.days);
    days
      .iter_mut()
      .filter(|(_, counters)| counters.pending != UsageCounts::default())
      .map(|(server_id, counters)| {
        (
          server_id.clone(),
          counters.day,
          std::mem::take(&mut counters.pending),
        )
      })
      .collect()
  }

  /// At most `per_minute` pushes per device token and minute. The token is only hashed here.
  pub fn admit_token(&self, token: &str, per_minute: u32, now_ms: u64) -> Result<(), Limited> {
    let key = self.token_hasher.hash_one(token);
    let mut tokens = lock(&self.tokens);
    let window = tokens.entry(key).or_insert(Window {
      started_ms: now_ms,
      count: 0,
    });
    if now_ms.saturating_sub(window.started_ms) >= MINUTE_MS {
      *window = Window {
        started_ms: now_ms,
        count: 0,
      };
    }
    if window.count >= per_minute {
      return Err(Limited::Token {
        retry_after_secs: secs_ceil(MINUTE_MS - now_ms.saturating_sub(window.started_ms)),
      });
    }
    window.count += 1;
    Ok(())
  }

  /// Refuses a request ID seen within `window_ms`; remembers it otherwise.
  pub fn admit_request_id(
    &self,
    server_id: &str,
    request_id: [u8; 16],
    window_ms: u64,
    now_ms: u64,
  ) -> Result<(), Limited> {
    let mut replay = lock(&self.replay);
    match replay.get(&(server_id.to_owned(), request_id)) {
      Some(&expires_ms) if expires_ms > now_ms => Err(Limited::Replay),
      _ => {
        replay.insert((server_id.to_owned(), request_id), now_ms + window_ms);
        Ok(())
      },
    }
  }

  /// A registration attempt from `address`; `per_address` and `global` are per hour.
  pub fn admit_registration(
    &self,
    address: IpAddr,
    per_address: u32,
    global: u32,
    now_ms: u64,
  ) -> Result<(), Limited> {
    let mut windows = lock(&self.registrations);
    let key = address_key(address);
    let entry = windows.per_address.entry(key).or_insert(Window {
      started_ms: now_ms,
      count: 0,
    });
    if now_ms.saturating_sub(entry.started_ms) >= HOUR_MS {
      *entry = Window {
        started_ms: now_ms,
        count: 0,
      };
    }
    let retry_after_secs = secs_ceil(HOUR_MS - now_ms.saturating_sub(entry.started_ms));
    if entry.count >= per_address {
      return Err(Limited::ServerRate { retry_after_secs });
    }
    let global_window = windows.global.get_or_insert(Window {
      started_ms: now_ms,
      count: 0,
    });
    if now_ms.saturating_sub(global_window.started_ms) >= HOUR_MS {
      *global_window = Window {
        started_ms: now_ms,
        count: 0,
      };
    }
    if global_window.count >= global {
      return Err(Limited::ServerRate { retry_after_secs });
    }
    global_window.count += 1;
    if let Some(entry) = windows.per_address.get_mut(&address_key(address)) {
      entry.count += 1;
    }
    Ok(())
  }

  /// Drops what has expired, so the maps follow the traffic instead of growing.
  pub fn sweep(&self, now_ms: u64) {
    lock(&self.replay).retain(|_, expires_ms| *expires_ms > now_ms);
    let mut tokens = lock(&self.tokens);
    tokens.retain(|_, window| now_ms.saturating_sub(window.started_ms) < MINUTE_MS);
    if tokens.len() > MAX_TRACKED_TOKENS {
      tokens.clear();
    }
    drop(tokens);
    lock(&self.registrations)
      .per_address
      .retain(|_, window| now_ms.saturating_sub(window.started_ms) < HOUR_MS);
    // A full bucket is the same as no bucket.
    lock(&self.buckets).retain(|_, bucket| now_ms.saturating_sub(bucket.updated_ms) < MINUTE_MS);
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
  Accepted,
  Gone,
  Failed,
}

/// IPv4 per address, IPv6 per /64 (one customer usually holds a whole /64).
fn address_key(address: IpAddr) -> String {
  match address {
    IpAddr::V4(v4) => v4.to_string(),
    IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
      Some(v4) => v4.to_string(),
      None => {
        let segments = v6.segments();
        format!(
          "{:x}:{:x}:{:x}:{:x}::/64",
          segments[0], segments[1], segments[2], segments[3]
        )
      },
    },
  }
}

fn secs_ceil(ms: u64) -> u32 {
  u32::try_from(ms.div_ceil(1000)).unwrap_or(u32::MAX).max(1)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
  use super::*;

  const LIMITS: ServerLimits = ServerLimits {
    pushes_per_second: 2,
    burst: 3,
    pushes_per_day: 5,
  };

  #[test]
  fn a_burst_then_the_steady_rate() {
    let limiter = Limiter::default();
    for _ in 0..3 {
      limiter.admit_server("s", LIMITS, 1, 0).unwrap();
    }
    assert!(matches!(
      limiter.admit_server("s", LIMITS, 1, 0),
      Err(Limited::ServerRate { retry_after_secs: 1 })
    ));
    // Half a second refills one push at 2 per second.
    limiter.admit_server("s", LIMITS, 1, 500).unwrap();
    assert!(limiter.admit_server("s", LIMITS, 1, 500).is_err());
  }

  #[test]
  fn servers_have_separate_buckets() {
    let limiter = Limiter::default();
    for _ in 0..3 {
      limiter.admit_server("a", LIMITS, 1, 0).unwrap();
    }
    limiter.admit_server("b", LIMITS, 1, 0).unwrap();
  }

  #[test]
  fn the_daily_quota_counts_what_the_store_already_holds() {
    let limiter = Limiter::default();
    assert!(!limiter.is_day_loaded("s", 1));
    limiter.seed_day("s", 1, 4);
    assert!(limiter.is_day_loaded("s", 1));
    limiter.admit_server("s", LIMITS, 1, 0).unwrap();
    limiter.record("s", Outcome::Accepted);
    assert!(matches!(
      limiter.admit_server("s", LIMITS, 1, 10_000),
      Err(Limited::DailyQuota { .. })
    ));
    // The next day starts again.
    assert!(!limiter.is_day_loaded("s", 2));
    limiter.seed_day("s", 2, 0);
    limiter.admit_server("s", LIMITS, 2, DAY_MS).unwrap();
  }

  #[test]
  fn pending_counters_are_drained_once() {
    let limiter = Limiter::default();
    limiter.admit_server("s", LIMITS, 1, 0).unwrap();
    limiter.record("s", Outcome::Accepted);
    limiter.record("s", Outcome::Gone);
    let drained = limiter.drain_pending();
    assert_eq!(drained.len(), 1);
    assert_eq!(
      drained[0].2,
      UsageCounts {
        accepted: 1,
        gone: 1,
        failed: 0
      }
    );
    assert!(limiter.drain_pending().is_empty());
  }

  #[test]
  fn one_token_gets_a_limited_number_per_minute() {
    let limiter = Limiter::default();
    for _ in 0..2 {
      limiter.admit_token("token", 2, 0).unwrap();
    }
    assert!(matches!(
      limiter.admit_token("token", 2, 1000),
      Err(Limited::Token { .. })
    ));
    limiter.admit_token("other", 2, 1000).unwrap();
    limiter.admit_token("token", 2, MINUTE_MS).unwrap();
  }

  #[test]
  fn a_request_id_is_accepted_once_per_window() {
    let limiter = Limiter::default();
    limiter.admit_request_id("s", [1; 16], 1000, 0).unwrap();
    assert_eq!(
      limiter.admit_request_id("s", [1; 16], 1000, 500),
      Err(Limited::Replay)
    );
    limiter.admit_request_id("s", [2; 16], 1000, 500).unwrap();
    limiter.admit_request_id("t", [1; 16], 1000, 500).unwrap();
    limiter.admit_request_id("s", [1; 16], 1000, 1500).unwrap();
  }

  #[test]
  fn registrations_are_limited_per_address_and_in_total() {
    let limiter = Limiter::default();
    let a: IpAddr = "192.0.2.1".parse().unwrap();
    let b: IpAddr = "192.0.2.2".parse().unwrap();
    limiter.admit_registration(a, 1, 2, 0).unwrap();
    assert!(limiter.admit_registration(a, 1, 2, 1).is_err());
    limiter.admit_registration(b, 1, 2, 2).unwrap();
    let c: IpAddr = "192.0.2.3".parse().unwrap();
    assert!(limiter.admit_registration(c, 1, 2, 3).is_err());
    limiter.admit_registration(c, 1, 2, HOUR_MS + 3).unwrap();
  }

  #[test]
  fn ipv6_is_limited_per_64_and_mapped_addresses_count_as_ipv4() {
    let one: IpAddr = "2001:db8:1:2::1".parse().unwrap();
    let other: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
    assert_eq!(address_key(one), address_key(other));
    let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
    assert_eq!(address_key(mapped), "192.0.2.1");
  }

  #[test]
  fn the_sweep_forgets_what_expired() {
    let limiter = Limiter::default();
    limiter.admit_request_id("s", [1; 16], 1000, 0).unwrap();
    limiter.admit_token("t", 1, 0).unwrap();
    limiter.sweep(2 * MINUTE_MS);
    assert!(lock(&limiter.replay).is_empty());
    assert!(lock(&limiter.tokens).is_empty());
  }
}
