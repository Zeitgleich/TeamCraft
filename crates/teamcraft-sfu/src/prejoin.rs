//! Per-address limit on sessions that have connected but not joined yet (C7): each one holds a
//! connection for up to the join timeout before any token has been checked. An IPv6 client usually
//! has a whole /64 to itself, so that is what counts as one address.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, PoisonError};

/// Counts pre-join sessions per key: a source address, or on the fallback also the device of a join
/// token checked before the upgrade (#1297).
#[derive(Debug)]
pub struct PreJoinLimiter<K = IpAddr> {
  counts: Mutex<HashMap<K, usize>>,
}

/// Holds one slot until dropped (the session joined or ended).
#[derive(Debug)]
pub struct PreJoinSlot<K: Eq + Hash + Copy = IpAddr> {
  limiter: Arc<PreJoinLimiter<K>>,
  key: K,
}

impl<K> Default for PreJoinLimiter<K> {
  fn default() -> Self {
    Self {
      counts: Mutex::new(HashMap::new()),
    }
  }
}

impl PreJoinLimiter<IpAddr> {
  /// Takes a slot for `ip`, or `None` if it already has `max` unjoined sessions.
  pub fn try_acquire(self: &Arc<Self>, ip: IpAddr, max: usize) -> Option<PreJoinSlot> {
    self.try_acquire_key(counted_address(ip), max)
  }
}

impl<K: Eq + Hash + Copy> PreJoinLimiter<K> {
  /// Takes a slot for `key`, or `None` if it already has `max` unjoined sessions.
  pub fn try_acquire_key(self: &Arc<Self>, key: K, max: usize) -> Option<PreJoinSlot<K>> {
    let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
    let count = counts.entry(key).or_insert(0);
    if *count >= max {
      return None;
    }
    *count += 1;
    Some(PreJoinSlot {
      limiter: Arc::clone(self),
      key,
    })
  }
}

impl<K: Eq + Hash + Copy> Drop for PreJoinSlot<K> {
  fn drop(&mut self) {
    let mut counts = self.limiter.counts.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(count) = counts.get_mut(&self.key) {
      *count -= 1;
      if *count == 0 {
        counts.remove(&self.key);
      }
    }
  }
}

/// What a limit counts: an IPv4 address (also written as IPv4-mapped) or an IPv6 /64.
fn counted_address(ip: IpAddr) -> IpAddr {
  match ip.to_canonical() {
    IpAddr::V6(ip) => {
      let network = u128::from(ip) & (u128::MAX << 64);
      IpAddr::V6(Ipv6Addr::from(network))
    },
    ipv4 => ipv4,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn slots_are_limited_per_address_and_freed_on_drop() {
    let limiter = Arc::new(PreJoinLimiter::default());
    let a: IpAddr = "192.0.2.1".parse().unwrap();
    let b: IpAddr = "2001:db8::1".parse().unwrap();
    let first = limiter.try_acquire(a, 2).unwrap();
    let _second = limiter.try_acquire(a, 2).unwrap();
    assert!(limiter.try_acquire(a, 2).is_none());
    assert!(limiter.try_acquire(b, 2).is_some());
    drop(first);
    assert!(limiter.try_acquire(a, 2).is_some());
  }

  #[test]
  fn an_ipv6_slash_64_is_one_address() {
    let limiter = Arc::new(PreJoinLimiter::default());
    let first = limiter
      .try_acquire("2001:db8:1:1::1".parse().unwrap(), 1)
      .unwrap();
    assert!(
      limiter
        .try_acquire("2001:db8:1:1:ffff::2".parse().unwrap(), 1)
        .is_none()
    );
    assert!(
      limiter
        .try_acquire("2001:db8:1:2::1".parse().unwrap(), 1)
        .is_some()
    );
    drop(first);
    assert!(
      limiter
        .try_acquire("2001:db8:1:1:ffff::2".parse().unwrap(), 1)
        .is_some()
    );
    assert!(limiter.counts.lock().unwrap().is_empty());
  }

  #[test]
  fn an_ipv4_address_counts_the_same_when_mapped() {
    let limiter = Arc::new(PreJoinLimiter::default());
    let _slot = limiter.try_acquire("192.0.2.1".parse().unwrap(), 1).unwrap();
    assert!(
      limiter
        .try_acquire("::ffff:192.0.2.1".parse().unwrap(), 1)
        .is_none()
    );
  }
}
