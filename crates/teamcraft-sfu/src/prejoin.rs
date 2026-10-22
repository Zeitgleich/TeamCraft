//! Per-IP limit on sessions that have connected but not joined yet (C7): each one holds a QUIC
//! connection for up to the join timeout before any token has been checked.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};

/// Counts pre-join sessions per source address.
#[derive(Debug, Default)]
pub struct PreJoinLimiter {
  counts: Mutex<HashMap<IpAddr, usize>>,
}

/// Holds one slot until dropped (the session joined or ended).
#[derive(Debug)]
pub struct PreJoinSlot {
  limiter: Arc<PreJoinLimiter>,
  ip: IpAddr,
}

impl PreJoinLimiter {
  /// Takes a slot for `ip`, or `None` if it already has `max` unjoined sessions.
  pub fn try_acquire(self: &Arc<Self>, ip: IpAddr, max: usize) -> Option<PreJoinSlot> {
    let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
    let count = counts.entry(ip).or_insert(0);
    if *count >= max {
      return None;
    }
    *count += 1;
    Some(PreJoinSlot {
      limiter: Arc::clone(self),
      ip,
    })
  }
}

impl Drop for PreJoinSlot {
  fn drop(&mut self) {
    let mut counts = self.limiter.counts.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(count) = counts.get_mut(&self.ip) {
      *count -= 1;
      if *count == 0 {
        counts.remove(&self.ip);
      }
    }
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
}
