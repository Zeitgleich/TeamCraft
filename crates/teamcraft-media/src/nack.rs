//! The receiver's side of video retransmission (#1762): which packets of a received layer are
//! missing, when to ask the SFU for them (a NACK) and when to give up on them.
//!
//! A packet is asked for as soon as a gap shows it missing, again after about one and a half
//! round trips while it doesn't come, at most [`NackConfig::max_requests`] times; after the last
//! request has had its round trip, or once it is older than the SFU keeps packets, it is lost and
//! the picture needs a keyframe as before. The round trip is measured on the answers themselves:
//! the time from the only request for a packet to its arrival (requests sent twice say nothing
//! about which one was answered).
//!
//! Sequence numbers are extended to 64 bits by the caller, so ordering needs no wrap handling.
//! Audio has no NACKs: Opus conceals and corrects losses itself, and a resent frame would come too
//! late to be played.

use std::collections::BTreeMap;
use std::time::Duration;

// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

/// The most missing packets one layer tracks; a larger gap is a loss at once (a keyframe is
/// cheaper than resending that much, and one NACK names no more).
pub const MAX_MISSING_PACKETS: usize = teamcraft_proto::MAX_NACK_SEQUENCE_NUMBERS;
/// What a retry waits beyond one and a half round trips: the SFU's pacing and the receiver's
/// scheduling.
const RETRY_MARGIN: Duration = Duration::from_millis(20);
/// A round-trip sample counts with this weight (RFC 6298's 1/8).
const RTT_SMOOTHING: f64 = 0.125;

/// How long packets are waited for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NackConfig {
  /// How long the SFU keeps packets (`joined`): nothing older is asked for or waited for.
  pub window: Duration,
  /// How often one packet is asked for at most.
  pub max_requests: u8,
  /// The round trip assumed until one is measured.
  pub initial_rtt: Duration,
}

/// The packets of one received layer that are missing.
#[derive(Debug)]
pub struct MissingPackets {
  /// `None`: the SFU resends nothing (an older one); gaps are only tracked.
  maybe_config: Option<NackConfig>,
  missing: BTreeMap<u64, MissingPacket>,
  maybe_rtt: Option<Duration>,
}

#[derive(Debug, Clone, Copy)]
struct MissingPacket {
  since: Instant,
  maybe_last_request: Option<Instant>,
  requests: u8,
}

/// What became of a packet that arrived after a gap had shown it missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LateArrival {
  /// It was asked for: a retransmission (or the original, late, after the request).
  Recovered,
  /// It was missing but not asked for yet (reordered).
  Reordered,
  /// It wasn't missing: a duplicate, or given up on already.
  Unexpected,
}

impl NackConfig {
  /// The receiver's settings for an SFU that keeps packets for `window`.
  pub fn for_window(window: Duration) -> Self {
    Self {
      window,
      max_requests: 3,
      initial_rtt: Duration::from_millis(150),
    }
  }
}

impl MissingPackets {
  pub fn new(maybe_config: Option<NackConfig>) -> Self {
    Self {
      maybe_config,
      missing: BTreeMap::new(),
      maybe_rtt: None,
    }
  }

  /// Whether missing packets are asked for (and waited for).
  pub fn asks_for_packets(&self) -> bool {
    self.maybe_config.is_some()
  }

  /// `sequence_number` was skipped at `now`. Returns `false`, tracking nothing, when that would
  /// track more than [`MAX_MISSING_PACKETS`].
  pub fn add(&mut self, sequence_numbers: std::ops::Range<u64>, now: Instant) -> bool {
    let count = sequence_numbers.end.saturating_sub(sequence_numbers.start);
    if self.missing.len() as u64 + count > MAX_MISSING_PACKETS as u64 {
      return false;
    }
    for sequence_number in sequence_numbers {
      self.missing.entry(sequence_number).or_insert(MissingPacket {
        since: now,
        maybe_last_request: None,
        requests: 0,
      });
    }
    true
  }

  /// A packet came that a gap had skipped; a single request answered is a round-trip sample.
  pub fn arrived(&mut self, sequence_number: u64, now: Instant) -> LateArrival {
    let Some(packet) = self.missing.remove(&sequence_number) else {
      return LateArrival::Unexpected;
    };
    match (packet.requests, packet.maybe_last_request) {
      (0, _) | (_, None) => LateArrival::Reordered,
      (1, Some(requested)) => {
        self.sample_rtt(now.saturating_duration_since(requested));
        LateArrival::Recovered
      },
      _ => LateArrival::Recovered,
    }
  }

  /// The packets to ask for now (oldest first), counted as asked for.
  pub fn due_requests(&mut self, now: Instant) -> Vec<u64> {
    let Some(config) = self.maybe_config else {
      return Vec::new();
    };
    let retry_after = self.retry_after(&config);
    self
      .missing
      .iter_mut()
      .filter(|(_, packet)| {
        packet.requests < config.max_requests
          && packet
            .maybe_last_request
            .is_none_or(|last| now.saturating_duration_since(last) >= retry_after)
      })
      .map(|(&sequence_number, packet)| {
        packet.requests += 1;
        packet.maybe_last_request = Some(now);
        sequence_number
      })
      .collect()
  }

  /// Gives up on the packets that can't come any more: asked for as often as allowed and not
  /// answered within a round trip, or older than the SFU keeps them. Returns the newest given up;
  /// every packet older than it is given up as well (the picture needs a keyframe anyway, and
  /// nothing after a lost packet can be shown without one). `None` if none.
  pub fn expire(&mut self, now: Instant) -> Option<(u64, usize)> {
    let config = self.maybe_config?;
    let retry_after = self.retry_after(&config);
    let newest = self
      .missing
      .iter()
      .filter(|(_, packet)| {
        let is_answer_overdue = packet.requests >= config.max_requests
          && packet
            .maybe_last_request
            .is_some_and(|last| now.saturating_duration_since(last) >= retry_after);
        is_answer_overdue || now.saturating_duration_since(packet.since) >= config.window
      })
      .map(|(&sequence_number, _)| sequence_number)
      .next_back()?;
    Some((newest, self.remove_through(newest)))
  }

  /// Forgets the packets up to `sequence_number` (inclusive); returns how many there were.
  pub fn remove_through(&mut self, sequence_number: u64) -> usize {
    let kept = self.missing.split_off(&(sequence_number.saturating_add(1)));
    let removed = std::mem::replace(&mut self.missing, kept);
    removed.len()
  }

  /// Forgets every missing packet; returns how many there were.
  pub fn clear(&mut self) -> usize {
    std::mem::take(&mut self.missing).len()
  }

  /// The oldest packet still missing.
  pub fn oldest(&self) -> Option<u64> {
    self.missing.keys().next().copied()
  }

  pub fn is_empty(&self) -> bool {
    self.missing.is_empty()
  }

  /// The round trip in use: measured, or the configured guess.
  pub fn rtt(&self) -> Option<Duration> {
    self
      .maybe_rtt
      .or_else(|| self.maybe_config.map(|config| config.initial_rtt))
  }

  /// How long a request is given before it is sent again.
  fn retry_after(&self, config: &NackConfig) -> Duration {
    let rtt = self.maybe_rtt.unwrap_or(config.initial_rtt);
    rtt + rtt / 2 + RETRY_MARGIN
  }

  fn sample_rtt(&mut self, sample: Duration) {
    // A sample beyond the window is no answer to this request; it would only stall retries.
    let Some(config) = self.maybe_config.filter(|config| sample <= config.window) else {
      return;
    };
    let smoothed = match self.maybe_rtt {
      None => sample,
      Some(rtt) => rtt.mul_f64(1.0 - RTT_SMOOTHING) + sample.mul_f64(RTT_SMOOTHING),
    };
    self.maybe_rtt = Some(smoothed.min(config.window));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const WINDOW: Duration = Duration::from_secs(1);
  const RTT: Duration = Duration::from_millis(150);
  /// One and a half round trips and the margin.
  const RETRY: Duration = Duration::from_millis(245);

  mod requests {
    use super::*;

    #[test]
    fn a_gap_is_asked_for_at_once_and_again_after_a_round_trip() {
      let (mut missing, now) = tracker();
      assert!(missing.add(10..13, now));
      assert_eq!(missing.due_requests(now), vec![10, 11, 12]);
      assert!(
        missing.due_requests(now + RETRY / 2).is_empty(),
        "not again so soon"
      );
      assert_eq!(missing.due_requests(now + RETRY), vec![10, 11, 12]);
    }

    #[test]
    fn a_packet_is_asked_for_a_limited_number_of_times() {
      let (mut missing, mut now) = tracker();
      missing.add(5..6, now);
      let mut requests = 0;
      for _ in 0..10 {
        requests += missing.due_requests(now).len();
        now += RETRY;
      }
      assert_eq!(requests, 3);
    }

    #[test]
    fn nothing_is_asked_of_an_sfu_that_resends_nothing() {
      let now = Instant::now();
      let mut missing = MissingPackets::new(None);
      assert!(missing.add(1..3, now));
      assert!(missing.due_requests(now).is_empty());
      assert_eq!(
        missing.expire(now + Duration::from_secs(60)),
        None,
        "the caller decides"
      );
      assert!(!missing.asks_for_packets());
    }

    #[test]
    fn a_gap_larger_than_a_nack_can_name_is_not_tracked() {
      let (mut missing, now) = tracker();
      assert!(missing.add(0..MAX_MISSING_PACKETS as u64, now));
      assert!(!missing.add(1000..1001, now));
      assert_eq!(missing.clear(), MAX_MISSING_PACKETS);
    }
  }

  mod arrivals {
    use super::*;

    #[test]
    fn an_answer_is_recovered_and_measures_the_round_trip() {
      let (mut missing, now) = tracker();
      missing.add(1..2, now);
      missing.due_requests(now);
      let answered = now + Duration::from_millis(40);
      assert_eq!(missing.arrived(1, answered), LateArrival::Recovered);
      assert_eq!(missing.rtt(), Some(Duration::from_millis(40)));
      // The next retry comes after the measured round trip, not the guess.
      missing.add(2..3, answered);
      missing.due_requests(answered);
      assert_eq!(
        missing.due_requests(answered + Duration::from_millis(80)),
        vec![2]
      );
    }

    #[test]
    fn a_packet_asked_for_twice_measures_nothing() {
      let (mut missing, now) = tracker();
      missing.add(1..2, now);
      missing.due_requests(now);
      missing.due_requests(now + RETRY);
      assert_eq!(
        missing.arrived(1, now + RETRY + Duration::from_millis(5)),
        LateArrival::Recovered
      );
      assert_eq!(missing.rtt(), Some(RTT), "still the guess");
    }

    #[test]
    fn reordered_and_duplicate_packets_are_told_apart() {
      let (mut missing, now) = tracker();
      missing.add(1..3, now);
      assert_eq!(missing.arrived(1, now), LateArrival::Reordered);
      assert_eq!(missing.arrived(1, now), LateArrival::Unexpected);
      assert_eq!(missing.arrived(7, now), LateArrival::Unexpected);
      assert_eq!(missing.oldest(), Some(2));
    }
  }

  mod giving_up {
    use super::*;

    #[test]
    fn after_the_last_request_had_its_round_trip() {
      let (mut missing, mut now) = tracker();
      missing.add(4..6, now);
      for _ in 0..3 {
        assert_eq!(missing.expire(now), None);
        missing.due_requests(now);
        now += RETRY;
      }
      assert_eq!(missing.expire(now), Some((5, 2)));
      assert!(missing.is_empty());
    }

    #[test]
    fn once_older_than_the_sfu_keeps_packets() {
      let (mut missing, now) = tracker();
      let config = NackConfig {
        max_requests: 100,
        ..NackConfig::for_window(WINDOW)
      };
      missing.maybe_config = Some(config);
      missing.add(1..2, now);
      missing.due_requests(now);
      assert_eq!(missing.expire(now + WINDOW - Duration::from_millis(1)), None);
      assert_eq!(missing.expire(now + WINDOW), Some((1, 1)));
    }

    #[test]
    fn everything_older_than_a_lost_packet_goes_with_it() {
      let (mut missing, now) = tracker();
      missing.add(1..2, now);
      missing.add(9..10, now + WINDOW / 2);
      missing.add(20..21, now + WINDOW);
      // Only 1 is too old; the newer ones are still waited for.
      assert_eq!(missing.expire(now + WINDOW), Some((1, 1)));
      assert_eq!(missing.oldest(), Some(9));
      assert_eq!(missing.remove_through(15), 1);
      assert_eq!(missing.oldest(), Some(20));
    }
  }

  fn tracker() -> (MissingPackets, Instant) {
    (
      MissingPackets::new(Some(NackConfig::for_window(WINDOW))),
      Instant::now(),
    )
  }
}
