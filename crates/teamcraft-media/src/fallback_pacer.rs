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
//! rate, taken over [`DRAIN_SPAN`] at least (#1863), so that one of the short stalls TCP has on a
//! lossy link lowers it, not wipes it out. And a ping's round trip above the smallest one seen is what waits in the buffers on the way.
//!
//! - Waiting above [`PacingConfig::target_queue_delay`]: the rate drops below what got through, so
//!   the buffers empty within a round trip or two.
//! - A short run of late answers between two that weren't: the cuts it made are undone
//!   (#1845). That is a stall, not a backlog: TCP recovering a lost or reordered segment (at a low
//!   rate too few segments are in flight for a fast retransmit), a jittery link or a busy
//!   receiver, and what got through meanwhile measured the stall, not the link. Growing back only
//!   by [`RATE_GROWTH`] per answer, each such stall ratcheted the rate down to its floor on a link
//!   of several Mbit/s with 0.5 % loss. A link that really narrowed keeps the answers late longer.
//! - Below half of it: the rate grows by [`RATE_GROWTH`] per answer, so a link that recovered is
//!   used again within seconds. While little is sent the rate isn't held to it: it may grow up to a
//!   few times what got through recently, at least to [`PacingConfig::start_rate_bps`], so the
//!   SFU's layer allocation can still switch up and find out.
//! - No answer for a while: the rate drops with every ping that goes unanswered.
//!
//! A peer that says when it read each ping ([`DrainPacer::on_answer_received`], the SFU's pong to a
//! browser, #1890) lets the pacer judge this direction alone: what waits is the ping's one-way
//! delay above the smallest one, and what drained is counted when the peer read it. A round trip
//! also holds the answer's way back, behind everything the other direction sends: a camera's
//! upload next to a busy download was cut to what its little traffic drained, sat at the floor on
//! a good link, and the camera with it.
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
/// A run of at most this many late answers that ends on an empty path is a stall: its cuts are
/// undone. With a ping every 250 ms, about as long as TCP takes to recover a lost segment.
const MAX_STALL_ANSWERS: u8 = 3;
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
/// What answers waited on their way back is taken over this long, for the silence check.
const REVERSE_WINDOW: Duration = Duration::from_secs(2);
/// What got through recently, for the rate's ceiling while little is sent.
const DRAIN_WINDOW: Duration = Duration::from_secs(5);
/// Two answers closer than this measure the timer more than the path.
const MIN_DRAIN_INTERVAL: Duration = Duration::from_millis(50);
/// What the path drained is measured over at least this long (#1863): on a lossy link TCP stalls
/// for a few hundred milliseconds every second or so, and a ping's interval across one measured
/// next to nothing, which took the rate to its floor at every stall.
const DRAIN_SPAN: Duration = Duration::from_millis(800);
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
  /// The rate never drops below this, so that the path is still measured. Audio isn't held to the
  /// rate, but it counts: at most this much video goes out besides it. A voice needs about
  /// 50 kbit/s; at 300 kbit/s the pacer kept more than twice a collapsed link's 120 kbit/s in
  /// flight, and the voice came seconds late or the session was lost (#1871, #1890).
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
  /// Answers within [`DRAIN_SPAN`] and the newest one before, oldest first.
  answers: VecDeque<Answer>,
  round_trips: VecDeque<(Instant, Duration)>,
  /// One-way delays of answered pings (the peer's clock minus this one's, in microseconds: only
  /// differences count), within [`MIN_ROUND_TRIP_WINDOW`].
  forward_delays: VecDeque<(Instant, i64)>,
  /// This side's clock base for those delays.
  maybe_epoch: Option<Instant>,
  /// What answers waited on their way back, above the least, within [`REVERSE_WINDOW`].
  reverse_waits: VecDeque<(Instant, Duration)>,
  drains: VecDeque<(Instant, f64)>,
  /// Whether the previous answer was late (its wait above the target).
  was_late: bool,
  /// A run of late answers that began on an empty path: the rate before it, and how many.
  maybe_stall: Option<Stall>,
}

/// A run of late answers that may be a stall ([`DrainPacer`]): undone if it ends soon.
#[derive(Debug, Clone, Copy)]
struct Stall {
  rate_before_bps: f64,
  late_answers: u8,
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
      min_rate_bps: 64_000,
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
      answers: VecDeque::new(),
      round_trips: VecDeque::new(),
      forward_delays: VecDeque::new(),
      maybe_epoch: None,
      reverse_waits: VecDeque::new(),
      drains: VecDeque::new(),
      was_late: true,
      maybe_stall: None,
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
    self.maybe_epoch.get_or_insert(now);
    // An answer that waits on its way back says nothing of this direction.
    let reverse_wait = self.recent_reverse_wait(now);
    if let Some(oldest) = self.probes.front()
      && self.queue_delay(now.saturating_duration_since(oldest.sent_at))
        > 2 * self.config.target_queue_delay + reverse_wait
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
    let Some(probe) = self.take_probe(number) else {
      return;
    };
    let round_trip = now.saturating_duration_since(probe.sent_at);
    self.note_round_trip(round_trip, now);
    let maybe_drain_bps = self.note_drain(probe.written_bytes, now);
    let queue_delay = self.queue_delay(round_trip);
    self.adapt(queue_delay, maybe_drain_bps, now);
  }

  /// The peer answered the ping with this number, and read it at `received_us` on its clock
  /// (microseconds from any base, the same for every answer).
  pub fn on_answer_received(&mut self, number: u64, received_us: u64, now: Instant) {
    let Some(probe) = self.take_probe(number) else {
      return;
    };
    let round_trip = now.saturating_duration_since(probe.sent_at);
    self.note_round_trip(round_trip, now);
    let epoch = *self.maybe_epoch.get_or_insert(probe.sent_at);
    let sent_us =
      i64::try_from(probe.sent_at.saturating_duration_since(epoch).as_micros()).unwrap_or(i64::MAX);
    let delay_us = i64::try_from(received_us)
      .unwrap_or(i64::MAX)
      .saturating_sub(sent_us);
    while self
      .forward_delays
      .front()
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) > MIN_ROUND_TRIP_WINDOW)
    {
      self.forward_delays.pop_front();
    }
    self.forward_delays.push_back((now, delay_us));
    let least_us = self
      .forward_delays
      .iter()
      .map(|(_, delay)| *delay)
      .min()
      .unwrap_or(delay_us);
    let forward_wait = Duration::from_micros(u64::try_from(delay_us.saturating_sub(least_us)).unwrap_or(0));
    // What the answer waited on its way back: the round trip above its least, less this way's wait.
    let reverse_wait = self.queue_delay(round_trip).saturating_sub(forward_wait);
    self.reverse_waits.push_back((now, reverse_wait));
    while self
      .reverse_waits
      .front()
      .is_some_and(|(at, _)| now.saturating_duration_since(*at) > REVERSE_WINDOW)
    {
      self.reverse_waits.pop_front();
    }
    // Drained when the peer read it: on this side's clock, the ping's send plus its wait.
    let read_at = probe.sent_at + forward_wait;
    let maybe_drain_bps = self.note_drain(probe.written_bytes, read_at);
    self.adapt(forward_wait, maybe_drain_bps, now);
  }

  /// The ping with this number, and the older ones dropped: TCP keeps the order, so they were read
  /// too, and their answers won't come any more.
  fn take_probe(&mut self, number: u64) -> Option<Probe> {
    let index = self.probes.iter().position(|probe| probe.number == number)?;
    let probe = self.probes[index];
    self.probes.drain(..=index);
    Some(probe)
  }

  /// The most an answer waited on its way back lately (zero without one-way answers).
  fn recent_reverse_wait(&self, now: Instant) -> Duration {
    self
      .reverse_waits
      .iter()
      .filter(|(at, _)| now.saturating_duration_since(*at) <= REVERSE_WINDOW)
      .map(|(_, wait)| *wait)
      .max()
      .unwrap_or(Duration::ZERO)
  }

  fn adapt(&mut self, queue_delay: Duration, maybe_drain_bps: Option<f64>, now: Instant) {
    let target = self.config.target_queue_delay;
    let is_late = queue_delay > target;
    if is_late {
      self.note_late_answer();
      let backoff = if queue_delay > 3 * target {
        DRAIN_BACKOFF_FAST
      } else {
        DRAIN_BACKOFF
      };
      let lowered = maybe_drain_bps.map_or(self.rate_bps * backoff, |drain_bps| drain_bps * backoff);
      self.set_rate(self.rate_bps.min(lowered));
    } else if let Some(stall) = self.maybe_stall.take() {
      // A stall that ended: what it cut comes back.
      self.set_rate(self.rate_bps.max(stall.rate_before_bps));
    } else if queue_delay < target / 2 {
      let ceiling = self.ceiling_bps(now);
      self.set_rate((self.rate_bps * RATE_GROWTH).min(ceiling.max(self.rate_bps)));
    }
    self.was_late = is_late;
  }

  /// Counts a late answer into a possible stall: one after an answer in time starts one, and one
  /// late answer more than [`MAX_STALL_ANSWERS`] makes it a backlog.
  fn note_late_answer(&mut self) {
    match &mut self.maybe_stall {
      Some(stall) if stall.late_answers < MAX_STALL_ANSWERS => stall.late_answers += 1,
      Some(_) => self.maybe_stall = None,
      None if !self.was_late => {
        self.maybe_stall = Some(Stall {
          rate_before_bps: self.rate_bps,
          late_answers: 1,
        });
      },
      None => {},
    }
  }

  /// While little is sent, the rate may grow this far without having been used.
  fn ceiling_bps(&self, now: Instant) -> f64 {
    (self.recent_drain_bps(now) * IDLE_HEADROOM).max(f64::from(self.config.start_rate_bps))
  }

  /// The most the path drained within [`DRAIN_WINDOW`].
  fn recent_drain_bps(&self, now: Instant) -> f64 {
    self
      .drains
      .iter()
      .filter(|(at, _)| now.saturating_duration_since(*at) < DRAIN_WINDOW)
      .map(|(_, bps)| *bps)
      .fold(0.0, f64::max)
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

  /// What the path drained over the last [`DRAIN_SPAN`], or since the first answer if that is
  /// more recent and far enough back.
  fn note_drain(&mut self, written_bytes: u64, now: Instant) -> Option<f64> {
    let maybe_from = self
      .answers
      .iter()
      .rev()
      .find(|answer| now.saturating_duration_since(answer.at) >= DRAIN_SPAN)
      .or_else(|| {
        self
          .answers
          .front()
          .filter(|answer| now.saturating_duration_since(answer.at) >= MIN_DRAIN_INTERVAL)
      })
      .copied();
    let maybe_drain_bps = maybe_from.map(|from| {
      let elapsed = now.saturating_duration_since(from.at);
      written_bytes.saturating_sub(from.written_bytes) as f64 * 8.0 / elapsed.as_secs_f64()
    });
    self.answers.push_back(Answer {
      at: now,
      written_bytes,
    });
    while self
      .answers
      .get(1)
      .is_some_and(|second| now.saturating_duration_since(second.at) >= DRAIN_SPAN)
    {
      self.answers.pop_front();
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

    /// #1845: round trips that jump by 80–180 ms every few answers while the path drains all
    /// that is written (a jittery link, a busy receiver) are not this connection's backlog: the
    /// rate grows to the link instead of ratcheting down to its floor.
    #[test]
    fn delay_spikes_on_a_path_that_keeps_up_dont_cut_the_rate() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      pacer.set_rate(600_000.0);
      let mut at = start;
      for number in 0..80u64 {
        // The media written is what the rate allows, and all of it gets through.
        let sent = pacer.rate_bps() / 8 / 4;
        pacer.on_probe_sent(number, at);
        pacer.on_written(sent as usize, at);
        let spike = match number % 12 {
          5 => 180 * MS,
          6 => 80 * MS,
          _ => Duration::ZERO,
        };
        pacer.on_answer(number, at + BASE_ROUND_TRIP + spike);
        at += 250 * MS;
      }
      assert!(pacer.rate_bps() >= 3_000_000, "{}", pacer.rate_bps());
    }

    /// #1890: a camera's small upload next to a busy download. Its pings reach the peer at once,
    /// but the answers wait 100 to 400 ms behind the download's media. Judged by the round trip
    /// that held the rate at its floor; judged one way it stays where the path allows.
    #[test]
    fn answers_that_wait_on_their_way_back_dont_cut_the_upload() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      let mut at = start;
      for number in 0..120u64 {
        pacer.on_probe_sent(number, at);
        pacer.on_written(2_500, at);
        let back = Duration::from_millis(100 + (number * 37 % 300));
        let received_us = 20_000 + u64::try_from((at - start).as_micros()).unwrap();
        pacer.on_answer_received(number, received_us, at + BASE_ROUND_TRIP + back);
        at += 250 * MS;
      }
      assert!(
        pacer.rate_bps() >= PacingConfig::default().start_rate_bps,
        "{}",
        pacer.rate_bps()
      );
    }

    /// Judged one way, an upload that writes more than the path drains is still cut below what got
    /// through.
    #[test]
    fn an_upload_that_falls_behind_is_cut_one_way_too() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      let mut at = start;
      let mut backlog_bytes = 0.0;
      for number in 0..24u64 {
        let sent = 125_000u32;
        pacer.on_probe_sent(number, at);
        pacer.on_written(sent as usize, at);
        backlog_bytes += f64::from(sent) - 1_500_000.0 / 8.0 / 4.0;
        let waiting = Duration::from_secs_f64(backlog_bytes.max(0.0) * 8.0 / 1_500_000.0);
        let received_us = 20_000 + u64::try_from((at - start + waiting).as_micros()).unwrap();
        pacer.on_answer_received(number, received_us, at + BASE_ROUND_TRIP + waiting);
        at += 250 * MS;
      }
      assert!(pacer.rate_bps() < 1_500_000, "{}", pacer.rate_bps());
    }

    /// A path that drains less than is written while the delay grows still brings the rate down,
    /// spikes or not.
    #[test]
    fn a_path_that_falls_behind_still_cuts_the_rate() {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      pacer.set_rate(4_000_000.0);
      // 4 Mbit/s written, 1.5 Mbit/s drained: each answer comes later by what waits.
      let mut at = start;
      let mut backlog_bytes = 0.0;
      for number in 0..24u64 {
        let sent = 125_000u32;
        pacer.on_probe_sent(number, at);
        pacer.on_written(sent as usize, at);
        backlog_bytes += f64::from(sent) - 1_500_000.0 / 8.0 / 4.0;
        let waiting = Duration::from_secs_f64(backlog_bytes.max(0.0) * 8.0 / 1_500_000.0);
        pacer.on_answer(number, at + BASE_ROUND_TRIP + waiting);
        at += 250 * MS;
      }
      assert!(pacer.rate_bps() < 1_500_000, "{}", pacer.rate_bps());
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
      for number in 1..40u64 {
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

  /// #1863: a link that carries 5 Mbit/s but, at 0.5 % loss, stalls for 200–500 ms every half to
  /// one and a half seconds while TCP recovers a segment, as the test stack's lossy profile did.
  /// Pings sent before or during a stall are answered only after it.
  mod tcp_stalls {
    use super::*;

    const LINK_BPS: f64 = 5_000_000.0;
    /// What the path holds of a connection's data (TCP's window, filling the bottleneck's buffer):
    /// beyond it the writer waits.
    const PATH_WINDOW_BYTES: f64 = 128.0 * 1024.0;
    const TICK: Duration = Duration::from_millis(5);
    const DATAGRAM_BYTES: usize = 1_200;

    #[test]
    fn short_stalls_dont_hold_the_rate_down() {
      let rates = simulate(Duration::from_secs(60), LINK_BPS, 7);
      let sharp = rates.iter().filter(|bps| **bps >= 2_500_000).count();
      let median = median(&rates);
      // About what the link carries between its stalls, most of the time: enough for the share's
      // 720p layer.
      assert!(
        sharp * 10 >= rates.len() * 7,
        "{sharp}/{} samples >= 2.5 Mbit/s, median {median}",
        rates.len()
      );
      assert!(median >= 3_000_000, "median {median}");
      // But not far above what the link carries.
      assert!(median < 7_000_000, "median {median}");
    }

    #[test]
    fn a_link_that_narrowed_brings_the_rate_down_within_seconds() {
      // 5 Mbit/s with stalls, then 1 Mbit/s with stalls from 30 s on: below 1.5 Mbit/s from
      // 33 s on (a sample every 250 ms).
      let rates = simulate_narrowing(7);
      let after_narrowing = &rates[132..];
      assert!(
        after_narrowing.iter().all(|bps| *bps < 1_500_000),
        "{after_narrowing:?}"
      );
    }

    /// The rate at each ping, as the SFU's layer allocation reads it.
    fn simulate(duration: Duration, link_bps: f64, seed: u64) -> Vec<u32> {
      run(duration, |_| link_bps, seed)
    }

    fn simulate_narrowing(seed: u64) -> Vec<u32> {
      run(
        Duration::from_secs(45),
        |elapsed| {
          if elapsed < Duration::from_secs(30) {
            LINK_BPS
          } else {
            1_000_000.0
          }
        },
        seed,
      )
    }

    fn run(duration: Duration, link_bps: impl Fn(Duration) -> f64, seed: u64) -> Vec<u32> {
      let start = Instant::now();
      let mut pacer = DrainPacer::new(PacingConfig::default());
      let mut rng = seed;
      let mut next_stall = start + Duration::from_millis(500 + random(&mut rng) % 1000);
      let mut stall_until = start;
      let mut written = 0.0f64;
      let mut drained = 0.0f64;
      // Pings on the way: their number and what was written before them.
      let mut pings: VecDeque<(u64, f64)> = VecDeque::new();
      let mut answers: VecDeque<(u64, Instant)> = VecDeque::new();
      let mut next_ping = start;
      let mut number = 0u64;
      let mut rates = Vec::new();
      let mut now = start;
      while now < start + duration {
        if now >= next_stall {
          stall_until = now + Duration::from_millis(200 + random(&mut rng) % 300);
          next_stall = stall_until + Duration::from_millis(500 + random(&mut rng) % 1000);
        }
        if now >= stall_until {
          let tick_bytes = link_bps(now - start) / 8.0 * TICK.as_secs_f64();
          drained = (drained + tick_bytes).min(written);
        }
        while pings.front().is_some_and(|(_, before)| *before <= drained) {
          let (answered, _) = pings.pop_front().unwrap();
          answers.push_back((answered, now + BASE_ROUND_TRIP));
        }
        while answers.front().is_some_and(|(_, at)| *at <= now) {
          let (answered, _) = answers.pop_front().unwrap();
          pacer.on_answer(answered, now);
        }
        if now >= next_ping {
          pacer.on_probe_sent(number, now);
          pings.push_back((number, written));
          number += 1;
          next_ping += pacer.probe_interval();
          rates.push(pacer.rate_bps());
        }
        // Video is always there: as much as the pacer and the path's window let through.
        while pacer.wait_for(now).is_zero() && written - drained < PATH_WINDOW_BYTES {
          pacer.on_written(DATAGRAM_BYTES, now);
          written += (DATAGRAM_BYTES + FRAME_OVERHEAD_BYTES) as f64;
        }
        now += TICK;
      }
      rates
    }

    fn random(state: &mut u64) -> u64 {
      *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
      *state >> 33
    }

    fn median(values: &[u32]) -> u32 {
      let mut sorted = values.to_vec();
      sorted.sort_unstable();
      sorted[sorted.len() / 2]
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
