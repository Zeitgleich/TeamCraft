//! Keeping a call alive across network changes (ARCHITECTURE.md §6.5).
//!
//! Two layers, both driven by the call drivers:
//!
//! 1. **Connection migration.** The client's UDP socket is bound to the wildcard address, so
//!    after a switch (Wi-Fi to LTE, a new NAT mapping) its packets leave through the new route and
//!    the SFU validates the new path; the QUIC connection, the DAVE group and the RTP numbering
//!    simply continue. [`PathMonitor`] watches that the path is alive: any received UDP packet
//!    counts, keep-alive answers included.
//!    When the system reports a network change, the native driver also moves the connection to a
//!    fresh socket (`rebind`), for networks the old socket can't reach the SFU from.
//! 2. **Rejoin.** If the path stays silent (the old network is gone and migration didn't happen,
//!    or the browser can't migrate), or the SFU shuts down (a restart, a deploy: it closes with
//!    `SFU_GOING_AWAY_CLOSE_CODE`), the driver opens a new connection with a fresh join token and
//!    the engine rejoins the call ([`crate::call_engine::CallEngine::rejoin`]); the SFU keeps the
//!    device's slot for a grace period, a restarted SFU starts a fresh room. [`ReconnectSchedule`]
//!    paces the attempts with back-off and jitter, and gives up when the grace period is over.
//!
//! Every attempt connects with what its own join ticket says: the SFU's URL, and the SHA-256s of
//! its certificate where that is self-signed. A restarted SFU may come back with a new certificate,
//! which the fresh token's response names ([`rejoin_attempt`]).
//!
//! Pure state: time is passed in, so all of it is tested without a network.

use std::collections::hash_map::RandomState;
use std::fmt;
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::time::Duration;

use teamcraft_proto::CallJoinTokenResponse;
use thiserror::Error;
// `std::time` on native; the browser clock on wasm, where `std` has none.
use web_time::{Instant, SystemTime, UNIX_EPOCH};

/// Timing of keep-alives, loss detection and rejoining.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResilienceConfig {
  /// QUIC keep-alive from the client: keeps carrier NAT mappings (often dropped after 30 s of
  /// silence, some after 20 s) open in quiet calls, and makes the SFU answer regularly.
  pub keep_alive_interval: Duration,
  /// QUIC idle timeout; the shorter of the client's and the SFU's applies.
  pub idle_timeout: Duration,
  /// Nothing received for this long: the path is dead, rejoin without waiting for the idle
  /// timeout. Several keep-alive rounds, so one lost packet doesn't trigger it.
  pub stall_timeout: Duration,
  /// After the system reported a network change, the path has this long to show it migrated.
  pub handover_timeout: Duration,
  /// Browsers: a session nothing arrived on for this long is pinged (the SFU answers), so a quiet
  /// call still shows its path alive to a page that can't see QUIC keep-alives. Well below
  /// `stall_timeout`, so a few answers can go missing.
  pub echo_interval: Duration,
  /// How long to keep trying to rejoin when the SFU doesn't say how long it keeps the slot.
  pub default_reconnect_grace: Duration,
  /// Pause before the second rejoin attempt; it doubles up to `max_retry_interval`.
  pub first_retry_interval: Duration,
  pub max_retry_interval: Duration,
  /// After the SFU announced it is going away, the first rejoin attempt waits a random part of
  /// this, so the restarted SFU isn't hit by every client of every call at the same moment.
  pub going_away_spread: Duration,
}

/// Watches whether the connection's path is alive, from the count of received UDP packets.
#[derive(Debug, Clone)]
pub struct PathMonitor {
  received_packets: u64,
  last_activity_at: Instant,
  maybe_handover_deadline: Option<Instant>,
}

/// What [`PathMonitor::check`] concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathState {
  Alive,
  /// A network change was reported and nothing arrived since; waiting for migration.
  Migrating,
  /// Silent for too long: give up on this connection.
  Dead,
}

/// Paces rejoin attempts within the grace period.
#[derive(Debug, Clone)]
pub struct ReconnectSchedule {
  lost_at: Instant,
  grace: Duration,
  attempts: u32,
  next_attempt_at: Instant,
  jitter: Jitter,
}

/// Spreads retry times, so clients that lost the SFU at the same moment (its restart, an office's
/// network outage) don't come back in lockstep. Not for security: a small generator (SplitMix64).
#[derive(Debug, Clone)]
pub struct Jitter {
  state: u64,
}

/// What a client needs to join a call: from `teamcraft-server`'s join token response.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinTicket {
  /// WebTransport URL of the SFU, e.g. `https://sfu.example.com:4433/call`.
  pub sfu_url: String,
  pub token: String,
  /// SHA-256s of the SFU's self-signed certificates the connection accepts (development and test
  /// setups, `[calls] sfu_certificate_sha256`); empty uses the trusted roots.
  pub certificate_hashes: Vec<[u8; 32]>,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("getting a join token failed: {reason}")]
pub struct JoinTicketError {
  pub reason: String,
}

impl Default for ResilienceConfig {
  fn default() -> Self {
    Self {
      keep_alive_interval: Duration::from_secs(2),
      idle_timeout: Duration::from_secs(10),
      stall_timeout: Duration::from_secs(5),
      handover_timeout: Duration::from_secs(2),
      echo_interval: Duration::from_secs(1),
      default_reconnect_grace: Duration::from_secs(30),
      first_retry_interval: Duration::from_millis(250),
      max_retry_interval: Duration::from_secs(2),
      going_away_spread: Duration::from_secs(1),
    }
  }
}

impl ResilienceConfig {
  /// The timing for browsers. The page sees no answers to the browser's own keep-alives: what
  /// proves a session alive are the SFU's keep-alives, every 2 s, where the browser counts them,
  /// and the SFU's answers to the page's pings. After a network change one of them must arrive,
  /// so the window is a little longer than the keep-alive interval.
  pub fn browser() -> Self {
    Self {
      handover_timeout: Duration::from_secs(3),
      ..Self::default()
    }
  }
}

impl PathMonitor {
  pub fn new(now: Instant) -> Self {
    Self {
      received_packets: 0,
      last_activity_at: now,
      maybe_handover_deadline: None,
    }
  }

  /// The connection's received UDP packet count (it only grows); new packets prove the path.
  pub fn on_received_packets(&mut self, count: u64, now: Instant) {
    if count > self.received_packets {
      self.received_packets = count;
      self.last_activity_at = now;
      self.maybe_handover_deadline = None;
    }
  }

  /// A count that may include packets from before a network change: it moves the baseline new
  /// packets are measured against, but proves nothing (browsers read counts asynchronously).
  pub fn on_baseline_packets(&mut self, count: u64) {
    self.received_packets = self.received_packets.max(count);
  }

  /// Something arrived that proves the path, seen by the driver itself rather than counted by the
  /// transport (a browser's answer to its ping): the same as new packets.
  pub fn on_activity(&mut self, now: Instant) {
    self.last_activity_at = self.last_activity_at.max(now);
    self.maybe_handover_deadline = None;
  }

  /// The system reported a network change (another interface, address or route): the path must
  /// show life again soon, packets that arrived before don't count.
  pub fn on_network_changed(&mut self, now: Instant, config: &ResilienceConfig) {
    self.maybe_handover_deadline = Some(now + config.handover_timeout);
  }

  /// Whether a network change is waiting for the path to show life.
  pub fn is_migrating(&self) -> bool {
    self.maybe_handover_deadline.is_some()
  }

  /// The driver couldn't watch for a while (a suspended page): the silence it missed proves
  /// nothing, so the path gets its full time again from `now`.
  pub fn restart_silence(&mut self, now: Instant, config: &ResilienceConfig) {
    self.last_activity_at = self.last_activity_at.max(now);
    if let Some(deadline) = &mut self.maybe_handover_deadline {
      *deadline = (*deadline).max(now + config.handover_timeout);
    }
  }

  pub fn check(&self, now: Instant, config: &ResilienceConfig) -> PathState {
    if now.saturating_duration_since(self.last_activity_at) >= config.stall_timeout {
      return PathState::Dead;
    }
    match self.maybe_handover_deadline {
      Some(deadline) if now >= deadline => PathState::Dead,
      Some(_) => PathState::Migrating,
      None => PathState::Alive,
    }
  }
}

impl ReconnectSchedule {
  /// The connection was lost at `now`; the first attempt is due at once.
  pub fn new(now: Instant, grace: Duration) -> Self {
    Self::with_jitter(now, grace, Jitter::random())
  }

  /// [`Self::new`] with a given jitter source (tests).
  pub fn with_jitter(now: Instant, grace: Duration, jitter: Jitter) -> Self {
    Self {
      lost_at: now,
      grace,
      attempts: 0,
      next_attempt_at: now,
      jitter,
    }
  }

  /// The SFU closed the connection because it is going away (a restart, a deploy): the first
  /// attempt waits a random part of [`ResilienceConfig::going_away_spread`] instead of starting at
  /// once.
  pub fn spread_first_attempt(&mut self, now: Instant, config: &ResilienceConfig) {
    if self.attempts == 0 {
      self.next_attempt_at = now + config.going_away_spread.mul_f64(self.jitter.next_fraction());
    }
  }

  /// When the next attempt is due.
  pub fn next_attempt_at(&self) -> Instant {
    self.next_attempt_at
  }

  /// Records an attempt that failed at `now` and schedules the next one: exponential back-off
  /// with "equal jitter", between half and all of the interval.
  pub fn on_failed_attempt(&mut self, now: Instant, config: &ResilienceConfig) {
    let factor = 2u32.saturating_pow(self.attempts.min(16));
    let interval = config
      .first_retry_interval
      .saturating_mul(factor)
      .min(config.max_retry_interval);
    let jittered = interval / 2 + (interval / 2).mul_f64(self.jitter.next_fraction());
    self.attempts += 1;
    self.next_attempt_at = now + jittered;
  }

  /// Makes the next attempt due at once (the network came back).
  pub fn hurry(&mut self, now: Instant) {
    self.next_attempt_at = self.next_attempt_at.min(now);
  }

  /// The SFU gave up the slot by now: rejoining would be a new join, the call is over for us.
  pub fn has_expired(&self, now: Instant) -> bool {
    now.saturating_duration_since(self.lost_at) >= self.grace
  }

  pub fn attempts(&self) -> u32 {
    self.attempts
  }
}

impl Jitter {
  /// Seeded from the process's random hash keys and the wall clock: different on every device
  /// and every call. (The browser has no random hash keys; the clock's nanoseconds differ there.)
  pub fn random() -> Self {
    let mut hasher = RandomState::new().build_hasher();
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    hasher.write_u128(since_epoch.as_nanos());
    Self::seeded(hasher.finish())
  }

  /// A fixed sequence (tests).
  pub fn seeded(seed: u64) -> Self {
    Self { state: seed }
  }

  /// The next value in `[0, 1)`.
  fn next_fraction(&mut self) -> f64 {
    // SplitMix64 (Steele, Lea, Flood 2014).
    self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = self.state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    // The top 53 bits, exactly representable as an f64 fraction.
    (z >> 11) as f64 / (1u64 << 53) as f64
  }
}

/// The token response of `teamcraft-server` as a ticket; certificate hashes that aren't 32 bytes of
/// hex are left out (the server checks its configuration at start).
impl From<CallJoinTokenResponse> for JoinTicket {
  fn from(response: CallJoinTokenResponse) -> Self {
    Self {
      certificate_hashes: certificate_hashes_from_hex(&response.sfu_certificate_sha256),
      sfu_url: response.sfu_url,
      token: response.token,
    }
  }
}

// Join tokens don't belong in logs.
impl fmt::Debug for JoinTicket {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("JoinTicket")
      .field("sfu_url", &self.sfu_url)
      .field("token", &"[redacted]")
      .field("certificate_hashes", &self.certificate_hashes.len())
      .finish()
  }
}

/// One rejoin attempt: a fresh ticket from `maybe_fetch` (or `fallback`, the last one, without a
/// ticket source), then a connection made by `connect` with that ticket's URL and certificate
/// hashes. Returns the connection and the ticket it used, which becomes the call's current one.
pub async fn rejoin_attempt<T, E, Fetch, Connect, Connecting>(
  maybe_fetch: Option<Fetch>,
  fallback: JoinTicket,
  connect: Connect,
) -> Result<(T, JoinTicket), E>
where
  Fetch: Future<Output = Result<JoinTicket, JoinTicketError>>,
  Connect: FnOnce(JoinTicket) -> Connecting,
  Connecting: Future<Output = Result<T, E>>,
  E: From<JoinTicketError>,
{
  let ticket = match maybe_fetch {
    Some(fetch) => fetch.await?,
    None => fallback,
  };
  let connection = connect(ticket.clone()).await?;
  Ok((connection, ticket))
}

/// [`certificate_hash_from_hex`] for every entry; malformed ones are left out.
pub fn certificate_hashes_from_hex(texts: &[String]) -> Vec<[u8; 32]> {
  texts
    .iter()
    .filter_map(|text| certificate_hash_from_hex(text))
    .collect()
}

/// A certificate hash written as hex (colons allowed), as `serverCertificateHashes` and the
/// native driver take it; `None` unless it is exactly 32 bytes.
pub fn certificate_hash_from_hex(text: &str) -> Option<[u8; 32]> {
  let hex: Vec<u8> = text.bytes().filter(|byte| *byte != b':').collect();
  if hex.len() != 64 {
    return None;
  }
  let mut hash = [0u8; 32];
  for (byte, pair) in hash.iter_mut().zip(hex.chunks(2)) {
    *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
  }
  Some(hash)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn certificate_hashes_read_from_hex_with_or_without_colons() {
    let plain = "ab".repeat(32);
    let dotted = vec!["AB"; 32].join(":");
    assert_eq!(certificate_hash_from_hex(&plain), Some([0xab; 32]));
    assert_eq!(certificate_hash_from_hex(&dotted), Some([0xab; 32]));
    for bad in ["", "ab", &"zz".repeat(32), &"ab".repeat(33)] {
      assert_eq!(certificate_hash_from_hex(bad), None, "{bad}");
    }
  }

  mod path {
    use super::*;

    #[test]
    fn steady_packets_keep_the_path_alive() {
      let start = Instant::now();
      let mut monitor = PathMonitor::new(start);
      for second in 1..=20u64 {
        let now = start + Duration::from_secs(second);
        monitor.on_received_packets(second * 3, now);
        assert_eq!(monitor.check(now, &config()), PathState::Alive);
      }
    }

    #[test]
    fn silence_beyond_the_stall_timeout_is_a_dead_path() {
      let start = Instant::now();
      let mut monitor = PathMonitor::new(start);
      monitor.on_received_packets(10, start);
      assert_eq!(monitor.check(at(start, 4_900), &config()), PathState::Alive);
      // The same count again is no sign of life.
      monitor.on_received_packets(10, at(start, 4_900));
      assert_eq!(monitor.check(at(start, 5_000), &config()), PathState::Dead);
    }

    #[test]
    fn network_change_needs_new_packets_within_the_handover_timeout() {
      let start = Instant::now();
      let mut monitor = PathMonitor::new(start);
      monitor.on_received_packets(10, start);
      // Wi-Fi to LTE: the path migrates and keep-alive answers arrive on the new one.
      monitor.on_network_changed(at(start, 1_000), &config());
      assert_eq!(monitor.check(at(start, 1_500), &config()), PathState::Migrating);
      monitor.on_received_packets(11, at(start, 1_800));
      assert_eq!(monitor.check(at(start, 3_500), &config()), PathState::Alive);
      // Another change, and nothing comes back: rejoin long before the stall timeout.
      monitor.on_network_changed(at(start, 4_000), &config());
      assert_eq!(monitor.check(at(start, 6_000), &config()), PathState::Dead);
    }

    #[test]
    fn a_baseline_count_is_no_sign_of_life() {
      let start = Instant::now();
      let mut monitor = PathMonitor::new(start);
      monitor.on_received_packets(10, start);
      monitor.on_network_changed(at(start, 1_000), &config());
      monitor.on_baseline_packets(12);
      monitor.on_received_packets(12, at(start, 2_000));
      assert_eq!(monitor.check(at(start, 3_000), &config()), PathState::Dead);
    }

    #[test]
    fn activity_without_a_count_proves_the_path() {
      let start = Instant::now();
      let mut monitor = PathMonitor::new(start);
      monitor.on_activity(at(start, 4_000));
      assert_eq!(monitor.check(at(start, 8_000), &config()), PathState::Alive);
      monitor.on_network_changed(at(start, 8_500), &config());
      assert!(monitor.is_migrating());
      monitor.on_activity(at(start, 9_000));
      assert!(!monitor.is_migrating());
      assert_eq!(monitor.check(at(start, 13_900), &config()), PathState::Alive);
      assert_eq!(monitor.check(at(start, 14_000), &config()), PathState::Dead);
    }

    #[test]
    fn a_restarted_silence_gives_the_path_its_full_time_again() {
      let start = Instant::now();
      let mut monitor = PathMonitor::new(start);
      monitor.on_network_changed(at(start, 1_000), &config());
      // The page slept through the handover deadline and the stall timeout.
      monitor.restart_silence(at(start, 60_000), &config());
      assert_eq!(monitor.check(at(start, 61_000), &config()), PathState::Migrating);
      assert_eq!(monitor.check(at(start, 62_000), &config()), PathState::Dead);
      let mut quiet = PathMonitor::new(start);
      quiet.restart_silence(at(start, 60_000), &config());
      assert_eq!(quiet.check(at(start, 64_900), &config()), PathState::Alive);
      assert_eq!(quiet.check(at(start, 65_000), &config()), PathState::Dead);
    }
  }

  mod schedule {
    use super::*;

    #[test]
    fn attempts_back_off_up_to_the_maximum_with_jitter() {
      let start = Instant::now();
      let mut schedule = ReconnectSchedule::with_jitter(start, Duration::from_secs(30), Jitter::seeded(7));
      assert_eq!(schedule.next_attempt_at(), start);
      let mut now = start;
      for base_millis in [250, 500, 1000, 2000, 2000, 2000] {
        schedule.on_failed_attempt(now, &config());
        let interval = schedule.next_attempt_at() - now;
        let base = Duration::from_millis(base_millis);
        assert!(
          interval >= base / 2 && interval <= base,
          "{interval:?} not within half of {base:?}"
        );
        now = schedule.next_attempt_at();
      }
      assert_eq!(schedule.attempts(), 6);
    }

    #[test]
    fn clients_lost_together_retry_at_different_times() {
      let start = Instant::now();
      let retries: Vec<Instant> = (0..8)
        .map(|seed| {
          let mut schedule =
            ReconnectSchedule::with_jitter(start, Duration::from_secs(30), Jitter::seeded(seed));
          for _ in 0..3 {
            schedule.on_failed_attempt(start, &config());
          }
          schedule.next_attempt_at()
        })
        .collect();
      let mut distinct = retries.clone();
      distinct.sort();
      distinct.dedup();
      assert!(distinct.len() >= 6, "{retries:?}");
    }

    #[test]
    fn network_coming_back_makes_the_next_attempt_due() {
      let start = Instant::now();
      let mut schedule = ReconnectSchedule::with_jitter(start, Duration::from_secs(30), Jitter::seeded(1));
      for _ in 0..4 {
        schedule.on_failed_attempt(start, &config());
      }
      assert!(schedule.next_attempt_at() >= at(start, 1_000));
      schedule.hurry(at(start, 300));
      assert_eq!(schedule.next_attempt_at(), at(start, 300));
    }

    #[test]
    fn going_away_spreads_the_first_attempt() {
      let start = Instant::now();
      let firsts: Vec<Instant> = (0..8)
        .map(|seed| {
          let mut schedule =
            ReconnectSchedule::with_jitter(start, Duration::from_secs(30), Jitter::seeded(seed));
          schedule.spread_first_attempt(start, &config());
          schedule.next_attempt_at()
        })
        .collect();
      for first in &firsts {
        assert!(*first >= start && *first < at(start, 1_000), "{firsts:?}");
      }
      let mut distinct = firsts.clone();
      distinct.sort();
      distinct.dedup();
      assert!(distinct.len() >= 6, "{firsts:?}");
    }

    #[test]
    fn spreading_doesnt_delay_a_rejoin_already_under_way() {
      let start = Instant::now();
      let mut schedule = ReconnectSchedule::with_jitter(start, Duration::from_secs(30), Jitter::seeded(3));
      schedule.on_failed_attempt(start, &config());
      let due = schedule.next_attempt_at();
      schedule.spread_first_attempt(start, &config());
      assert_eq!(schedule.next_attempt_at(), due);
    }

    #[test]
    fn grace_period_ends_the_attempts() {
      let start = Instant::now();
      let schedule = ReconnectSchedule::new(start, Duration::from_secs(30));
      assert!(!schedule.has_expired(at(start, 29_999)));
      assert!(schedule.has_expired(at(start, 30_000)));
    }
  }

  #[test]
  fn jitter_fractions_stay_in_the_unit_interval() {
    let mut jitter = Jitter::seeded(0);
    for _ in 0..10_000 {
      let fraction = jitter.next_fraction();
      assert!((0.0..1.0).contains(&fraction), "{fraction}");
    }
    assert_ne!(
      Jitter::seeded(1).next_fraction(),
      Jitter::seeded(2).next_fraction()
    );
  }

  #[test]
  fn ticket_debug_output_hides_the_token() {
    let ticket = ticket("secret-token", [0xab; 32]);
    assert!(!format!("{ticket:?}").contains("secret-token"));
  }

  #[test]
  fn browsers_wait_for_one_sfu_keep_alive_after_a_network_change() {
    let browser = ResilienceConfig::browser();
    assert!(browser.handover_timeout > Duration::from_secs(2));
    assert!(browser.handover_timeout < browser.stall_timeout);
  }

  mod tickets {
    use teamcraft_proto::{CallId, Id, Snowflake, UnixMillis};

    use super::*;

    #[test]
    fn a_token_response_carries_its_certificate_hashes() {
      let response = CallJoinTokenResponse {
        call_id: CallId::from_snowflake(Snowflake::ZERO),
        token: "token".into(),
        sfu_url: "https://sfu.example.com:4433/call".into(),
        expires_at: UnixMillis(0),
        sfu_certificate_sha256: vec!["ab".repeat(32), "not hex".into(), vec!["CD"; 32].join(":")],
        ends_when_one_side_leaves: false,
      };
      let ticket = JoinTicket::from(response);
      assert_eq!(ticket.certificate_hashes, vec![[0xab; 32], [0xcd; 32]]);
      assert_eq!(ticket.sfu_url, "https://sfu.example.com:4433/call");
    }

    #[test]
    fn a_rejoin_connects_with_the_fresh_tickets_hashes() {
      // The SFU restarted with a new self-signed certificate: the first join's hash is stale.
      let first = ticket("first", [1; 32]);
      let fresh = ticket("fresh", [2; 32]);
      let fetch = std::future::ready(Ok(fresh.clone()));
      let result: Result<(Vec<[u8; 32]>, JoinTicket), JoinTicketError> =
        ready(rejoin_attempt(Some(fetch), first, |ticket: JoinTicket| {
          std::future::ready(Ok(ticket.certificate_hashes))
        }));
      let (connected_with, used) = result.unwrap();
      assert_eq!(connected_with, vec![[2; 32]]);
      assert_eq!(used, fresh);
    }

    #[test]
    fn without_a_ticket_source_the_last_ticket_is_used_again() {
      let last = ticket("last", [3; 32]);
      let result: Result<(Vec<[u8; 32]>, JoinTicket), JoinTicketError> = ready(rejoin_attempt(
        None::<std::future::Ready<Result<JoinTicket, JoinTicketError>>>,
        last.clone(),
        |ticket: JoinTicket| std::future::ready(Ok(ticket.certificate_hashes)),
      ));
      assert_eq!(result.unwrap(), (vec![[3; 32]], last));
    }

    #[test]
    fn a_refused_ticket_fails_the_attempt_without_connecting() {
      let refused = JoinTicketError {
        reason: "offline".into(),
      };
      let fetch = std::future::ready(Err(refused.clone()));
      let result: Result<((), JoinTicket), JoinTicketError> = ready(rejoin_attempt(
        Some(fetch),
        ticket("last", [3; 32]),
        |_: JoinTicket| -> std::future::Ready<Result<(), JoinTicketError>> {
          panic!("must not connect without a ticket")
        },
      ));
      assert_eq!(result.unwrap_err(), refused);
    }

    /// Runs a future that completes without waiting (every future in these tests does).
    fn ready<F: Future>(future: F) -> F::Output {
      let mut future = std::pin::pin!(future);
      let mut context = std::task::Context::from_waker(std::task::Waker::noop());
      match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(output) => output,
        std::task::Poll::Pending => panic!("the future waited"),
      }
    }
  }

  fn ticket(token: &str, hash: [u8; 32]) -> JoinTicket {
    JoinTicket {
      sfu_url: "https://sfu.example.com:4433/call".into(),
      token: token.into(),
      certificate_hashes: vec![hash],
    }
  }

  fn config() -> ResilienceConfig {
    ResilienceConfig::default()
  }

  fn at(start: Instant, millis: u64) -> Instant {
    start + Duration::from_millis(millis)
  }
}
