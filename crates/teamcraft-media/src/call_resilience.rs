//! Keeping a call alive across network changes (ARCHITECTURE.md §6.5).
//!
//! Two layers, both driven by the call drivers:
//!
//! 1. **Connection migration.** The client's UDP socket is bound to the wildcard address, so
//!    after a switch (Wi-Fi to LTE, a new NAT mapping) its packets leave through the new route and
//!    the SFU validates the new path; the QUIC connection, the DAVE group and the RTP numbering
//!    simply continue. [`PathMonitor`] watches that the path is alive: any received UDP packet
//!    counts, keep-alive answers included.
//! 2. **Rejoin.** If the path stays silent (the old network is gone and migration didn't happen,
//!    or the browser can't migrate), the driver opens a new connection with a fresh join token
//!    and the engine rejoins the call ([`crate::call_engine::CallEngine::rejoin`]); the SFU keeps
//!    the device's slot for a grace period. [`ReconnectSchedule`] paces the attempts and gives up
//!    when the grace period is over.
//!
//! Pure state: time is passed in, so all of it is tested without a network.

use std::fmt;
use std::time::Duration;

use thiserror::Error;
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

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
  /// How long to keep trying to rejoin when the SFU doesn't say how long it keeps the slot.
  pub default_reconnect_grace: Duration,
  /// Pause before the second rejoin attempt; it doubles up to `max_retry_interval`.
  pub first_retry_interval: Duration,
  pub max_retry_interval: Duration,
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
}

/// What a client needs to join a call: from `teamcraft-server`'s join token response.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinTicket {
  /// WebTransport URL of the SFU, e.g. `https://sfu.example.com:4433/call`.
  pub sfu_url: String,
  pub token: String,
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
      default_reconnect_grace: Duration::from_secs(30),
      first_retry_interval: Duration::from_millis(250),
      max_retry_interval: Duration::from_secs(2),
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

  /// The system reported a network change (another interface, address or route): the path must
  /// show life again soon, packets that arrived before don't count.
  pub fn on_network_changed(&mut self, now: Instant, config: &ResilienceConfig) {
    self.maybe_handover_deadline = Some(now + config.handover_timeout);
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
    Self {
      lost_at: now,
      grace,
      attempts: 0,
      next_attempt_at: now,
    }
  }

  /// When the next attempt is due.
  pub fn next_attempt_at(&self) -> Instant {
    self.next_attempt_at
  }

  /// Records an attempt that failed at `now` and schedules the next one.
  pub fn on_failed_attempt(&mut self, now: Instant, config: &ResilienceConfig) {
    let factor = 2u32.saturating_pow(self.attempts.min(16));
    let interval = config
      .first_retry_interval
      .saturating_mul(factor)
      .min(config.max_retry_interval);
    self.attempts += 1;
    self.next_attempt_at = now + interval;
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

// Join tokens don't belong in logs.
impl fmt::Debug for JoinTicket {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("JoinTicket")
      .field("sfu_url", &self.sfu_url)
      .field("token", &"[redacted]")
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

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
  }

  mod schedule {
    use super::*;

    #[test]
    fn attempts_back_off_up_to_the_maximum() {
      let start = Instant::now();
      let mut schedule = ReconnectSchedule::new(start, Duration::from_secs(30));
      assert_eq!(schedule.next_attempt_at(), start);
      let mut now = start;
      let mut intervals = Vec::new();
      for _ in 0..6 {
        schedule.on_failed_attempt(now, &config());
        intervals.push(schedule.next_attempt_at() - now);
        now = schedule.next_attempt_at();
      }
      let millis: Vec<u128> = intervals.iter().map(Duration::as_millis).collect();
      assert_eq!(millis, vec![250, 500, 1000, 2000, 2000, 2000]);
      assert_eq!(schedule.attempts(), 6);
    }

    #[test]
    fn network_coming_back_makes_the_next_attempt_due() {
      let start = Instant::now();
      let mut schedule = ReconnectSchedule::new(start, Duration::from_secs(30));
      for _ in 0..4 {
        schedule.on_failed_attempt(start, &config());
      }
      assert_eq!(schedule.next_attempt_at(), at(start, 2_000));
      schedule.hurry(at(start, 300));
      assert_eq!(schedule.next_attempt_at(), at(start, 300));
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
  fn ticket_debug_output_hides_the_token() {
    let ticket = JoinTicket {
      sfu_url: "https://sfu.example.com:4433/call".into(),
      token: "secret-token".into(),
    };
    assert!(!format!("{ticket:?}").contains("secret-token"));
  }

  fn config() -> ResilienceConfig {
    ResilienceConfig::default()
  }

  fn at(start: Instant, millis: u64) -> Instant {
    start + Duration::from_millis(millis)
  }
}
