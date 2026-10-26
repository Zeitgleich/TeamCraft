//! Happy Eyeballs (RFC 8305) for the native clients (issue #420): connection attempts to a host's
//! addresses race, with the address families interleaved and the next attempt started after
//! [`ATTEMPT_DELAY`] or as soon as the previous one failed. A broken IPv6 route (packets swallowed,
//! no error) then costs 250 ms before IPv4 is tried, not a whole connect timeout.
//!
//! Transport-neutral: the core races TCP connects for its WebSockets, the call driver QUIC
//! handshakes to the SFU. reqwest (REST) has its own racing in hyper-util.
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream::FuturesUnordered;

/// RFC 8305 §5: the next address is tried this long after the previous attempt started.
pub const ATTEMPT_DELAY: Duration = Duration::from_millis(250);

/// Why no attempt connected.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RaceError<E> {
  #[error("the host has no address")]
  NoAddress,
  /// Every attempt failed; the first error, usually the most telling one.
  #[error("{0}")]
  Failed(E),
}

/// Alternates the address families, starting with the resolver's first choice (RFC 8305 §4), so a
/// dead family costs one attempt delay and not one attempt per address.
pub fn interleave(addresses: Vec<SocketAddr>) -> Vec<SocketAddr> {
  let Some(first) = addresses.first() else {
    return addresses;
  };
  let prefers_v6 = first.is_ipv6();
  let (mut preferred, mut other): (Vec<SocketAddr>, Vec<SocketAddr>) = addresses
    .iter()
    .partition(|address| address.is_ipv6() == prefers_v6);
  preferred.reverse();
  other.reverse();
  let mut ordered = Vec::with_capacity(addresses.len());
  loop {
    match (preferred.pop(), other.pop()) {
      (None, None) => return ordered,
      (maybe_a, maybe_b) => ordered.extend(maybe_a.into_iter().chain(maybe_b)),
    }
  }
}

/// Runs `attempt` for `addresses` in order (interleave them first), starting the next one after
/// `delay` or when the previous one failed, and returns the first that succeeds. The losers are
/// dropped, which cancels them.
pub async fn race<T, E, F, Fut>(
  addresses: Vec<SocketAddr>,
  delay: Duration,
  mut attempt: F,
) -> Result<T, RaceError<E>>
where
  F: FnMut(SocketAddr) -> Fut,
  Fut: Future<Output = Result<T, E>>,
{
  let mut pending = addresses.into_iter();
  let mut attempts = FuturesUnordered::new();
  let mut maybe_first_error = None;
  loop {
    if attempts.is_empty() {
      match pending.next() {
        Some(address) => attempts.push(attempt(address)),
        None => return Err(maybe_first_error.map_or(RaceError::NoAddress, RaceError::Failed)),
      }
    }
    tokio::select! {
      Some(result) = attempts.next() => match result {
        Ok(connection) => return Ok(connection),
        Err(error) => {
          maybe_first_error.get_or_insert(error);
          if let Some(address) = pending.next() {
            attempts.push(attempt(address));
          }
        },
      },
      () = tokio::time::sleep(delay), if pending.len() > 0 => {
        if let Some(address) = pending.next() {
          attempts.push(attempt(address));
        }
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use std::net::{Ipv4Addr, Ipv6Addr};

  use tokio::time::Instant;

  use super::*;

  #[test]
  fn addresses_alternate_families_starting_with_the_first() {
    assert_eq!(
      interleave(vec![v6(1), v6(2), v6(3), v4(1)]),
      vec![v6(1), v4(1), v6(2), v6(3)]
    );
    assert_eq!(interleave(vec![v4(1), v6(1), v4(2)]), vec![v4(1), v6(1), v4(2)]);
    assert!(interleave(Vec::new()).is_empty());
  }

  /// The live failure of 1.0.2: IPv6 swallows packets, IPv4 works. IPv4 wins one attempt delay
  /// later, while the IPv6 attempt still hangs.
  #[tokio::test(start_paused = true)]
  async fn a_black_holed_ipv6_falls_back_to_ipv4_after_the_attempt_delay() {
    let start = Instant::now();
    let winner = race(interleave(vec![v6(1), v4(1)]), ATTEMPT_DELAY, black_hole_v6).await;
    assert_eq!(winner, Ok(v4(1)));
    assert_eq!(start.elapsed(), ATTEMPT_DELAY + CONNECT_TIME);
  }

  #[tokio::test(start_paused = true)]
  async fn a_refused_address_starts_the_next_at_once() {
    let start = Instant::now();
    let refused = |address: SocketAddr| async move {
      if address == v6(1) {
        Err("refused")
      } else {
        tokio::time::sleep(CONNECT_TIME).await;
        Ok(address)
      }
    };
    assert_eq!(race(vec![v6(1), v4(1)], ATTEMPT_DELAY, refused).await, Ok(v4(1)));
    assert_eq!(start.elapsed(), CONNECT_TIME);
  }

  #[tokio::test(start_paused = true)]
  async fn the_first_error_is_kept_when_all_fail() {
    let failing = |address: SocketAddr| async move { Err::<(), _>(address) };
    assert_eq!(
      race(vec![v6(1), v4(1)], ATTEMPT_DELAY, failing).await,
      Err(RaceError::Failed(v6(1)))
    );
    assert_eq!(
      race(Vec::new(), ATTEMPT_DELAY, failing).await,
      Err(RaceError::NoAddress)
    );
  }

  #[tokio::test(start_paused = true)]
  async fn a_working_first_address_wins_without_waiting() {
    let start = Instant::now();
    let winner = race(vec![v4(1), v6(1)], ATTEMPT_DELAY, black_hole_v6).await;
    assert_eq!(winner, Ok(v4(1)));
    assert_eq!(start.elapsed(), CONNECT_TIME);
  }

  const CONNECT_TIME: Duration = Duration::from_millis(20);

  /// IPv6 attempts never finish; IPv4 ones connect after [`CONNECT_TIME`].
  async fn black_hole_v6(address: SocketAddr) -> Result<SocketAddr, &'static str> {
    if address.is_ipv6() {
      std::future::pending::<()>().await;
    }
    tokio::time::sleep(CONNECT_TIME).await;
    Ok(address)
  }

  fn v6(n: u16) -> SocketAddr {
    SocketAddr::from((Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, n), 443))
  }

  fn v4(n: u8) -> SocketAddr {
    SocketAddr::from((Ipv4Addr::new(192, 0, 2, n), 443))
  }
}
