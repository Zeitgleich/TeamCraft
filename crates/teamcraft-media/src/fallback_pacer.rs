//! Paces the media of a call's WebSocket fallback to what the path actually drains (#1810).
//!
//! TCP doesn't drop: what a writer hands it beyond what the link carries waits in the kernel's send
//! buffer, in a reverse proxy's buffers and in the network, where [`crate::fallback_queue`] can't
//! drop it any more. On a link that swings between 1.5 and 9 Mbit/s that was seconds of media,
//! with the pings and the voice behind it, until the client's silence watch gave the session up.
//! So the writer sends media no faster than the path drains, and keeps the rest in its own queue,
//! where stale video is dropped and audio overtakes.
//!
//! The drain rate is measured end to end, through any proxy: every ping goes out behind all that was
//! written before it, and the peer's WebSocket answers it only once it has read all of that. So
//! between two answers, the bytes written between their pings got through: that is the path's drain
//! rate. And a ping's round trip above the smallest one seen is what waits in the buffers on the way.
//!
//! - Waiting above [`PacingConfig::target_queue_delay`]: the rate drops below what got through, so
//!   the buffers empty within a round trip or two.
//! - Below half of it: the rate grows by [`RATE_GROWTH`] per answer, so a link that recovered is
//!   used again within seconds. While little is sent the rate isn't held to it: it may grow up to a
//!   few times what got through recently, at least to [`PacingConfig::start_rate_bps`], so the
//!   SFU's layer allocation can still switch up and find out.
//! - No answer for a while: the rate drops with every ping that goes unanswered.
//!
//! Audio, signaling and pings are never held back (the writer sends them regardless), but they
//! count: video waits for what they took.
//!
//! Pure state: time is passed in, so all of it is tested without a network.

use std::collections::VecDeque;
use std::time::Duration;

// `std::time` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

/// The rate grows by this factor per answer while nothing waits on the path: from 1.5 to 9 Mbit/s
/// in about four seconds at four answers a second.
const RATE_GROWTH: f64 = 1.12;
/// Below what got through when the path's buffers fill, so that they empty.
const DRAIN_BACKOFF: f64 = 0.9;
/// When they hold several times the target already.
const DRAIN_BACKOFF_FAST: f64 = 0.7;
/// Per ping while the oldest one has waited too long for its answer.
const SILENCE_BACKOFF: f64 = 0.8;
/// While little is sent, the rate may be this many times what got through recently.
const IDLE_HEADROOM: f64 = 4.0;
/// Pings remembered until their answer; older ones count as answered with a later one (a browser
/// may answer only the latest ping it read).
const MAX_PENDING_PROBES: usize = 64;
/// The smallest round trip is taken over this long: long enough to see the path empty now and then,
/// short enough to follow a path that changed.
const MIN_ROUND_TRIP_WINDOW: Duration = Duration::from_secs(10);
/// What got through recently, for the rate's ceiling while little is sent.
const DRAIN_WINDOW: Duration = Duration::from_secs(5);
/// Two answers closer than this measure the timer more than the path.
const MIN_DRAIN_INTERVAL: Duration = Duration::from_millis(50);
/// The smallest burst: a couple of full datagrams.
const MIN_BURST_BYTES: f64 = 4.0 * 1024.0;
/// What a WebSocket frame adds to its payload, about: its header, without a mask from the SFU.
pub const FRAME_OVERHEAD_BYTES: usize = 4;

/// How the fallback paces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PacingConfig {
  /// How often the path is measured: a ping each time. Its answers are the samples, so this is how
  /// fast the rate follows the link.
  pub probe_interval: Duration,
  /// The rate before anything was measured, and the least the ceiling grows to while little is sent.
  pub start_rate_bps: u32,
  /// The rate never drops below this, so audio and the lowest video layer can still go out and be
  /// measured.
  pub min_rate_bps: u32,
  pub max_rate_bps: u32,
  /// How much may wait in the buffers on the path, measured as the round trip above its smallest.
  pub target_queue_delay: Duration,
  /// How much may go out at once, as time at the current rate.
  pub burst: Duration,
}

/// The pacing state of one connection.
#[derive(Debug)]
pub struct DrainPacer {
  config: PacingConfig,
  rate_bps: f64,
  tokens_bytes: f64,
  maybe_refilled_at: Option<Instant>,
  /// Everything written so far, in bytes.
  written_bytes: u64,
  /// Pings without an answer yet, oldest first.
  probes: VecDeque<Probe>,
  maybe_last_answer: Option<Answer>,
  round_trips: VecDeque<(Instant, Duration)>,
  drains: VecDeque<(Instant, f64)>,
}

#[derive(Debug, Clone, Copy)]
struct Probe {
  number: u64,
  sent_at: Instant,
  /// What was written before it.
  written_bytes: u64,
}

#[derive(Debug, Clone, Copy)]
struct Answer {
  at: Instant,
  written_bytes: u64,
}

impl Default for PacingConfig {
  fn default() -> Self {
    Self {
      probe_interval: Duration::from_millis(250),
      start_rate_bps: 4_000_000,
      min_rate_bps: 300_000,
      max_rate_bps: 100_000_000,
      target_queue_delay: Duration::from_millis(120),
      burst: Duration::from_millis(20),
    }
  }
}

impl DrainPacer {
  pub fn new(config: PacingConfig) -> Self {
    Self {
      config,
      rate_bps: f64::from(config.start_rate_bps),
      tokens_bytes: 0.0,
      maybe_refilled_at: None,
      written_bytes: 0,
      probes: VecDeque::new(),
      maybe_last_answer: None,
      round_trips: VecDeque::new(),
      drains: VecDeque::new(),
    }
  }

  /// The rate media is paced to, in bits per second.
  pub fn rate_bps(&self) -> u32 {
    // Invariant: clamped to `max_rate_bps`, a `u32`.
    self.rate_bps as u32
  }

  pub fn probe_interval(&self) -> Duration {
    self.config.probe_interval
  }

  /// How long until video may go again: zero now.
  pub fn wait_for(&mut self, now: Instant) -> Duration {
    self.refill(now);
    if self.tokens_bytes >= 0.0 {
      return Duration::ZERO;
    }
    Duration::from_secs_f64(-self.tokens_bytes / self.rate_bytes_per_second())
  }

  /// Something of this many bytes was written: media, signaling or a ping.
  pub fn on_written(&mut self, bytes: usize, now: Instant) {
    self.refill(now);
    let bytes = bytes + FRAME_OVERHEAD_BYTES;
    self.tokens_bytes -= bytes as f64;
    self.written_bytes += bytes as u64;
  }

  /// A ping with this number was written just now, behind everything written before.
  pub fn on_probe_sent(&mut self, number: u64, now: Instant) {
    if let Some(oldest) = self.probes.front()
      && self.queue_delay(now.saturating_duration_since(oldest.sent_at)) > 2 * self.config.target_queue_delay
    {
      // Nothing came back for a while: what was sent is stuck somewhere on the way.
      self.set_rate(self.rate_bps * SILENCE_BACKOFF);
    }
    self.probes.push_back(Probe {
      number,
      sent_at: now,
      written_bytes: self.written_bytes,
    });
    while self.probes.len() > MAX_PENDING_PROBES {
      self.probes.pop_front();
    }
  }

  /// The peer answered the ping with this number.
  pub fn on_answer(&mut self, number: u64, now: Instant) {
    let Some(index) = self.probes.iter().position(|probe| probe.number == number) else {
      return;
    };
    let probe = self.probes[index];
    // TCP keeps the order: older pings were read too, and their answers won't come any more.
    self.probes.drain(..=index);
    let round_trip = now.saturating_duration_since(probe.sent_at);
    self.note_round_trip(round_trip, now);
    let maybe_drain_bps = self.note_drain(probe.written_bytes, now);
    self.adapt(self.queue_delay(round_trip), maybe_drain_bps, now);
  }

  fn adapt(&mut self, queue_delay: Duration, maybe_drain_bps: Option<f64>, now: Instant) {
    let target = self.config.target_queue_delay;
    if queue_delay > target {
      let backoff = if queue_delay > 3 * target {
        DRAIN_BACKOFF_FAST
      } else {
        DRAIN_BACKOFF
      };
      let lowered = maybe_drain_bps.map_or(self.rate_bps * backoff, |drain_bps| drain_bps * backoff);
      self.set_rate(self.rate_bps.min(lowered));
    } else if queue_delay < target / 2 {
      let ceiling = self.ceiling_bps(now);
      self.set_rate((self.rate_bps * RATE_GROWTH).min(ceiling.max(self.rate_bps)));
    }
  }

  /// While little is sent, the rate may grow this far without having been used.
  fn ceiling_bps(&self, now: Instant) -> f64 {
    let recent_max = self
      .drains
      .iter()
      .filter(|(at, _)| now.saturating_duration_since(*at) < DRAIN_WINDOW)
      .map(|(_, bps)| *bps)
      .fold(0.0, f64::max);
    (recent_max * IDLE_HEADROOM).max(f64::from(self.config.start_rate_bps))
  }

  fn note_round_trip(&mut self, round_trip: Duration, now: Instant) {
    self.round_trips.push_back((now, round_trip));
    while self
      .round_trips
      .front()
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) > MIN_ROUND_TRIP_WINDOW)
    {
      self.round_trips.pop_front();
    }
  }

  /// The drain rate between the last answer and this one, if they are far enough apart.
  fn note_drain(&mut self, written_bytes: u64, now: Instant) -> Option<f64> {
    let maybe_last = self.maybe_last_answer;
    let maybe_drain_bps = maybe_last.and_then(|last| {
      let elapsed = now.saturating_duration_since(last.at);
      (elapsed >= MIN_DRAIN_INTERVAL)
        .then(|| written_bytes.saturating_sub(last.written_bytes) as f64 * 8.0 / elapsed.as_secs_f64())
    });
    if maybe_last.is_none() || maybe_drain_bps.is_some() {
      self.maybe_last_answer = Some(Answer {
        at: now,
        written_bytes,
      });
    }
    if let Some(drain_bps) = maybe_drain_bps {
      self.drains.push_back((now, drain_bps));
      while self
        .drains
        .front()
        .is_some_and(|(at, _)| now.saturating_duration_since(*at) > DRAIN_WINDOW)
      {
        self.drains.pop_front();
      }
    }
    maybe_drain_bps
  }

  /// How much of a round trip is waiting in the buffers on the way.
  fn queue_delay(&self, round_trip: Duration) -> Duration {
    let min = self
      .round_trips
      .iter()
      .map(|(_, round_trip)| *round_trip)
      .min()
      .unwrap_or(round_trip);
    round_trip.saturating_sub(min)
  }

  fn set_rate(&mut self, rate_bps: f64) {
    self.rate_bps = rate_bps.clamp(
      f64::from(self.config.min_rate_bps),
      f64::from(self.config.max_rate_bps),
    );
  }

  fn refill(&mut self, now: Instant) {
    let elapsed = self
      .maybe_refilled_at
      .map_or(Duration::ZERO, |at| now.saturating_duration_since(at));
    self.maybe_refilled_at = Some(now);
    let burst = (self.rate_bytes_per_second() * self.config.burst.as_secs_f64()).max(MIN_BURST_BYTES);
    self.tokens_bytes = (self.tokens_bytes + self.rate_bytes_per_second() * elapsed.as_secs_f64()).min(burst);
  }

  fn rate_bytes_per_second(&self) -> f64 {
    self.rate_bps / 8.0
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const MS: Duration = Duration::from_millis(1);
  const BASE_ROUND_TRIP: Duration = Duration::from_millis(40);

  mod rate {
    use super::*;

    #[test]
    fn a_filling_path_brings_the_rate_below_what_got_through() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      // 2 Mbit/s got through, and the round trip grows with every ping.
      let mut at = start;
      for number in 0..20u64 {
        pacer.on_probe_sent(number, at);
        pacer.on_written(62_500 - FRAME_OVERHEAD_BYTES, at);
        let round_trip = BASE_ROUND_TRIP + 50 * MS * u32::try_from(number).unwrap();
        pacer.on_answer(number, at + round_trip);
        at += 250 * MS;
      }
      assert!(pacer.rate_bps() < 2_000_000, "{}", pacer.rate_bps());
      assert!(pacer.rate_bps() >= PacingConfig::default().min_rate_bps);
    }

    #[test]
    fn an_empty_path_lets_the_rate_grow_within_seconds() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      pacer.set_rate(1_500_000.0);
      // The link carries what is sent, up to 9 Mbit/s; nothing waits.
      let mut at = start;
      for number in 0..16u64 {
        let sent = pacer.rate_bps() / 8 / 4;
        pacer.on_probe_sent(number, at);
        pacer.on_written(sent as usize, at);
        pacer.on_answer(number, at + BASE_ROUND_TRIP);
        at += 250 * MS;
      }
      assert!(pacer.rate_bps() > 6_000_000, "{}", pacer.rate_bps());
    }

    #[test]
    fn little_traffic_doesnt_hold_the_rate_below_the_start() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      let mut at = start;
      for number in 0..100u64 {
        pacer.on_probe_sent(number, at);
        pacer.on_written(1000, at);
        pacer.on_answer(number, at + BASE_ROUND_TRIP);
        at += 250 * MS;
      }
      assert!(pacer.rate_bps() >= PacingConfig::default().start_rate_bps);
      // But it doesn't grow without bound either.
      assert!(pacer.rate_bps() < 10_000_000, "{}", pacer.rate_bps());
    }

    #[test]
    fn unanswered_pings_lower_the_rate() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      pacer.on_probe_sent(0, start);
      pacer.on_answer(0, start + BASE_ROUND_TRIP);
      for number in 1..20u64 {
        pacer.on_probe_sent(number, start + 250 * MS * u32::try_from(number).unwrap());
      }
      assert_eq!(pacer.rate_bps(), PacingConfig::default().min_rate_bps);
    }

    #[test]
    fn an_answer_to_a_later_ping_settles_the_earlier_ones() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      for number in 0..3u64 {
        pacer.on_probe_sent(number, start + MS * u32::try_from(number).unwrap());
      }
      pacer.on_answer(2, start + BASE_ROUND_TRIP);
      assert!(pacer.probes.is_empty());
      // An unknown or repeated answer changes nothing.
      let rate = pacer.rate_bps();
      pacer.on_answer(1, start + BASE_ROUND_TRIP);
      pacer.on_answer(99, start + BASE_ROUND_TRIP);
      assert_eq!(pacer.rate_bps(), rate);
    }

    #[test]
    fn pending_pings_stay_bounded() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      for number in 0..1000u64 {
        pacer.on_probe_sent(number, start);
      }
      assert_eq!(pacer.probes.len(), MAX_PENDING_PROBES);
    }
  }

  mod tokens {
    use super::*;

    #[test]
    fn video_waits_for_what_was_written_beyond_the_rate() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig {
        start_rate_bps: 800_000,
        ..PacingConfig::default()
      });
      assert_eq!(pacer.wait_for(start), Duration::ZERO);
      // 100 kB/s: 10 kB more than the bucket held is 100 ms.
      pacer.on_written(10_000 - FRAME_OVERHEAD_BYTES, start);
      assert!(pacer.wait_for(start).abs_diff(100 * MS) < MS);
      assert!(pacer.wait_for(start + 100 * MS) < MS);
      assert_eq!(pacer.wait_for(start + 101 * MS), Duration::ZERO);
    }

    #[test]
    fn an_idle_while_saves_up_no_more_than_a_burst() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig {
        start_rate_bps: 8_000_000,
        ..PacingConfig::default()
      });
      pacer.wait_for(start);
      // 20 ms at 1 MB/s.
      pacer.on_written(20_000 - FRAME_OVERHEAD_BYTES, start + Duration::from_secs(60));
      assert_eq!(pacer.wait_for(start + Duration::from_secs(60)), Duration::ZERO);
      pacer.on_written(1000 - FRAME_OVERHEAD_BYTES, start + Duration::from_secs(60));
      assert!(pacer.wait_for(start + Duration::from_secs(60)) > Duration::ZERO);
    }
  }
}
