//! Send-side bandwidth estimation from transport-wide feedback (a simplified Google Congestion
//! Control, draft-ietf-rmcat-gcc).
//!
//! The sender numbers every datagram with a [`TransportSeq`] (datagram header) and remembers when it
//! sent it. The receiver records arrival times ([`FeedbackRecorder`]) and reports them about every
//! 100 ms over the signaling stream. The sender then combines two signals:
//!
//! - **Delay:** if packets arrive increasingly later than they were sent (a queue builds up), a
//!   trendline over the one-way delay variation turns positive. Overuse cuts the rate to 85 % of
//!   what was actually delivered.
//! - **Loss:** above 10 % loss the rate shrinks with the loss; between 2 % and 10 % it holds.
//!
//! Otherwise the rate grows multiplicatively (8 %/s by default), capped at 1.5× the delivered rate
//! so an application-limited sender doesn't drift upwards, unless the sender declares a higher
//! demand ([`SendSideEstimator::set_demand_bps`]): then the estimate may grow up to it, and sending
//! more (e.g. the SFU switching to the next simulcast layer) is the probe. A probe that causes
//! congestion is cut back by the delay detector like any other overuse. The same estimator runs in clients
//! (uplink) and in the SFU per receiver (downlink). Time is passed in; nothing reads a clock.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use teamcraft_proto::TransportFeedback;

use crate::datagram::TransportSeq;

/// Packets sent within this interval are one group for the delay detector (GCC: 5 ms).
const BURST_INTERVAL: Duration = Duration::from_millis(5);
/// Packet groups kept in the trendline regression.
const TRENDLINE_WINDOW: usize = 20;
/// Smoothing of the accumulated delay before the regression.
const DELAY_SMOOTHING: f64 = 0.9;
/// Gain applied to the trendline slope (GCC uses 4).
const TRENDLINE_GAIN: f64 = 4.0;
/// Adaptive threshold bounds and rates (GCC values, in ms).
const THRESHOLD_INITIAL_MS: f64 = 12.5;
const THRESHOLD_MIN_MS: f64 = 6.0;
const THRESHOLD_MAX_MS: f64 = 600.0;
const THRESHOLD_UP_RATE: f64 = 0.0087;
const THRESHOLD_DOWN_RATE: f64 = 0.039;
/// Window over which the delivered (acknowledged) bitrate is measured.
const ACKED_RATE_WINDOW: Duration = Duration::from_millis(500);
/// Sent packets older than this without feedback are forgotten.
const SEND_HISTORY: Duration = Duration::from_secs(5);
/// Minimum time between two delay-based decreases (about one feedback round trip).
const DELAY_DECREASE_INTERVAL: Duration = Duration::from_millis(200);
/// Window over which the loss ratio is measured.
const LOSS_WINDOW: Duration = Duration::from_secs(1);
/// Largest feedback the recorder produces in one message.
const MAX_FEEDBACK_ENTRIES: usize = 1000;

/// Settings of the estimator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BweConfig {
  pub start_bps: u32,
  pub min_bps: u32,
  pub max_bps: u32,
  /// Multiplicative growth per second while the path is not congested.
  pub increase_per_second: f64,
  /// Factor applied to the delivered rate on overuse.
  pub decrease_factor: f64,
}

/// Sender side: numbers datagrams and turns feedback into a target bitrate.
#[derive(Debug)]
pub struct SendSideEstimator {
  config: BweConfig,
  next_seq: TransportSeq,
  sent: BTreeMap<u64, SentPacket>,
  highest_sent: u64,
  delay: DelayDetector,
  acked: VecDeque<(Instant, usize)>,
  estimate_bps: f64,
  maybe_last_update_at: Option<Instant>,
  maybe_last_decrease_at: Option<Instant>,
  /// (feedback time, lost, reported) of the last [`LOSS_WINDOW`].
  loss_history: VecDeque<(Instant, usize, usize)>,
  last_loss_ratio: f64,
  maybe_demand_bps: Option<u32>,
  /// After a delay-based decrease, no further one until the detector reported normal again (the
  /// queue needs time to drain; cutting on every report would spiral down).
  is_holding_after_decrease: bool,
}

/// Receiver side: records arrival times and produces [`TransportFeedback`].
#[derive(Debug)]
pub struct FeedbackRecorder {
  epoch: Instant,
  arrivals: BTreeMap<u64, u64>,
  maybe_next_to_report: Option<u64>,
  highest_received: u64,
}

/// Congestion signal of the delay detector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BandwidthUsage {
  Normal,
  Overusing,
  Underusing,
}

#[derive(Debug, Clone, Copy)]
struct PacketGroup {
  first_sent: Instant,
  last_sent: Instant,
  last_arrival: Instant,
}

#[derive(Debug, Clone, Copy)]
struct SentPacket {
  at: Instant,
  bytes: usize,
}

#[derive(Debug)]
struct DelayDetector {
  /// Packets sent within [`BURST_INTERVAL`] form one group (first send, last send, last arrival);
  /// comparing groups instead of packets ignores the spacing a burst gets on the bottleneck.
  maybe_group: Option<PacketGroup>,
  /// (last send, last arrival) of the previous complete group.
  maybe_previous: Option<(Instant, Instant)>,
  maybe_first_arrival: Option<Instant>,
  accumulated_ms: f64,
  smoothed_ms: f64,
  window: VecDeque<(f64, f64)>,
  samples: usize,
  threshold_ms: f64,
  maybe_last_threshold_update: Option<Instant>,
  usage: BandwidthUsage,
}

impl Default for BweConfig {
  fn default() -> Self {
    Self {
      start_bps: 300_000,
      min_bps: 30_000,
      max_bps: 10_000_000,
      increase_per_second: 0.08,
      decrease_factor: 0.85,
    }
  }
}

impl SendSideEstimator {
  pub fn new(config: BweConfig) -> Self {
    Self {
      config,
      next_seq: TransportSeq(0),
      sent: BTreeMap::new(),
      highest_sent: 0,
      delay: DelayDetector::new(),
      acked: VecDeque::new(),
      estimate_bps: f64::from(config.start_bps),
      maybe_last_update_at: None,
      maybe_last_decrease_at: None,
      loss_history: VecDeque::new(),
      last_loss_ratio: 0.0,
      maybe_demand_bps: None,
      is_holding_after_decrease: false,
    }
  }

  /// Numbers the next datagram of `bytes` bytes sent at `now`.
  pub fn on_send(&mut self, bytes: usize, now: Instant) -> TransportSeq {
    let seq = self.next_seq;
    self.next_seq = seq.next();
    let extended = if self.sent.is_empty() && self.highest_sent == 0 {
      u64::from(seq.0)
    } else {
      unwrap_seq(seq.0, self.highest_sent)
    };
    self.highest_sent = self.highest_sent.max(extended);
    self.sent.insert(extended, SentPacket { at: now, bytes });
    self.forget_old(now);
    seq
  }

  /// Applies one feedback message and returns the new estimate.
  pub fn on_feedback(&mut self, feedback: &TransportFeedback, now: Instant, receiver_epoch: Instant) -> u32 {
    let base = unwrap_seq(feedback.base_seq, self.highest_sent);
    let mut lost = 0usize;
    let mut reported = 0usize;
    for (offset, maybe_arrival_us) in feedback.arrivals_us.iter().enumerate() {
      let Some(sent) = self.sent.remove(&(base + offset as u64)) else {
        continue;
      };
      reported += 1;
      match maybe_arrival_us {
        Some(arrival_us) => {
          let arrival = receiver_epoch + Duration::from_micros(*arrival_us);
          self.delay.on_packet(sent.at, arrival, now);
          self.acked.push_back((arrival, sent.bytes));
        },
        None => lost += 1,
      }
    }
    if reported > 0 {
      self.loss_history.push_back((now, lost, reported));
    }
    self
      .loss_history
      .retain(|(at, _, _)| now.saturating_duration_since(*at) <= LOSS_WINDOW);
    let (lost_total, reported_total) = self
      .loss_history
      .iter()
      .fold((0, 0), |(lost_sum, reported_sum), (_, lost, reported)| {
        (lost_sum + lost, reported_sum + reported)
      });
    if reported_total > 0 {
      self.last_loss_ratio = lost_total as f64 / reported_total as f64;
    }
    self.update_rate(now);
    self.estimate_bps()
  }

  /// What the sender would like to send; lets the estimate grow past the delivered rate up to it.
  pub fn set_demand_bps(&mut self, maybe_demand_bps: Option<u32>) {
    self.maybe_demand_bps = maybe_demand_bps;
  }

  pub fn estimate_bps(&self) -> u32 {
    // Invariant: clamped to the u32 config bounds in `update_rate`.
    self.estimate_bps as u32
  }

  pub fn usage(&self) -> BandwidthUsage {
    self.delay.usage
  }

  pub fn loss_ratio(&self) -> f64 {
    self.last_loss_ratio
  }

  fn update_rate(&mut self, now: Instant) {
    let elapsed = self
      .maybe_last_update_at
      .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
    self.maybe_last_update_at = Some(now);
    let acked_bps = self.acked_bps();
    match self.delay.usage {
      BandwidthUsage::Overusing => {
        if acked_bps > 0.0
          && !self.is_holding_after_decrease
          && self.may_decrease(now, DELAY_DECREASE_INTERVAL)
        {
          self.estimate_bps = self.estimate_bps.min(self.config.decrease_factor * acked_bps);
          self.maybe_last_decrease_at = Some(now);
          self.is_holding_after_decrease = true;
        }
      },
      BandwidthUsage::Underusing => {},
      BandwidthUsage::Normal => {
        self.is_holding_after_decrease = false;
        if self.last_loss_ratio < 0.02 {
          let growth = (1.0 + self.config.increase_per_second).powf(elapsed.as_secs_f64());
          let app_limited_ceiling = if acked_bps > 0.0 {
            1.5 * acked_bps + 10_000.0
          } else {
            f64::MAX
          };
          let ceiling = app_limited_ceiling.max(self.maybe_demand_bps.map_or(0.0, f64::from));
          self.estimate_bps = (self.estimate_bps * growth).min(ceiling.max(self.estimate_bps));
        }
      },
    }
    if self.last_loss_ratio > 0.10 && acked_bps > 0.0 && self.may_decrease(now, LOSS_WINDOW) {
      // Based on what got through, not on the previous estimate, so repeated reports of the same
      // loss episode don't compound.
      let loss_based = acked_bps * (1.0 - 0.5 * self.last_loss_ratio);
      self.estimate_bps = self.estimate_bps.min(loss_based);
      self.maybe_last_decrease_at = Some(now);
    }
    self.estimate_bps = self
      .estimate_bps
      .clamp(f64::from(self.config.min_bps), f64::from(self.config.max_bps));
  }

  fn may_decrease(&self, now: Instant, interval: Duration) -> bool {
    self
      .maybe_last_decrease_at
      .is_none_or(|last| now.saturating_duration_since(last) >= interval)
  }

  /// Delivered bitrate over the last [`ACKED_RATE_WINDOW`] of arrivals.
  fn acked_bps(&mut self) -> f64 {
    let Some(&(latest, _)) = self.acked.iter().max_by_key(|(at, _)| *at) else {
      return 0.0;
    };
    self
      .acked
      .retain(|(at, _)| latest.saturating_duration_since(*at) <= ACKED_RATE_WINDOW);
    let bytes: usize = self.acked.iter().map(|(_, bytes)| bytes).sum();
    let Some(&(earliest, _)) = self.acked.iter().min_by_key(|(at, _)| *at) else {
      return 0.0;
    };
    let span = latest
      .saturating_duration_since(earliest)
      .max(Duration::from_millis(100));
    bytes as f64 * 8.0 / span.as_secs_f64()
  }

  fn forget_old(&mut self, now: Instant) {
    while let Some((&oldest, packet)) = self.sent.first_key_value() {
      if now.saturating_duration_since(packet.at) <= SEND_HISTORY {
        break;
      }
      self.sent.remove(&oldest);
    }
  }
}

impl DelayDetector {
  fn new() -> Self {
    Self {
      maybe_group: None,
      maybe_previous: None,
      maybe_first_arrival: None,
      accumulated_ms: 0.0,
      smoothed_ms: 0.0,
      window: VecDeque::new(),
      samples: 0,
      threshold_ms: THRESHOLD_INITIAL_MS,
      maybe_last_threshold_update: None,
      usage: BandwidthUsage::Normal,
    }
  }

  fn on_packet(&mut self, sent_at: Instant, arrived_at: Instant, now: Instant) {
    match &mut self.maybe_group {
      Some(group) if sent_at.saturating_duration_since(group.first_sent) <= BURST_INTERVAL => {
        group.last_sent = group.last_sent.max(sent_at);
        group.last_arrival = group.last_arrival.max(arrived_at);
      },
      _ => {
        if let Some(group) = self.maybe_group.take() {
          self.on_group(group.last_sent, group.last_arrival, now);
        }
        self.maybe_group = Some(PacketGroup {
          first_sent: sent_at,
          last_sent: sent_at,
          last_arrival: arrived_at,
        });
      },
    }
  }

  fn on_group(&mut self, sent_at: Instant, arrived_at: Instant, now: Instant) {
    let first_arrival = *self.maybe_first_arrival.get_or_insert(arrived_at);
    if let Some((previous_sent, previous_arrived)) = self.maybe_previous {
      let send_delta = signed_ms(sent_at, previous_sent);
      let arrival_delta = signed_ms(arrived_at, previous_arrived);
      self.accumulated_ms += arrival_delta - send_delta;
      self.smoothed_ms = DELAY_SMOOTHING * self.smoothed_ms + (1.0 - DELAY_SMOOTHING) * self.accumulated_ms;
      self
        .window
        .push_back((signed_ms(arrived_at, first_arrival), self.smoothed_ms));
      if self.window.len() > TRENDLINE_WINDOW {
        self.window.pop_front();
      }
      self.samples += 1;
      if self.window.len() == TRENDLINE_WINDOW {
        self.detect(now);
      }
    }
    self.maybe_previous = Some((sent_at, arrived_at));
  }

  fn detect(&mut self, now: Instant) {
    let slope = linear_slope(&self.window);
    let modified = slope * self.samples.min(60) as f64 * TRENDLINE_GAIN;
    self.usage = if modified > self.threshold_ms {
      BandwidthUsage::Overusing
    } else if modified < -self.threshold_ms {
      BandwidthUsage::Underusing
    } else {
      BandwidthUsage::Normal
    };
    self.adapt_threshold(modified.abs(), now);
  }

  /// The threshold follows the signal so it neither starves against TCP nor ignores congestion.
  fn adapt_threshold(&mut self, modified_abs: f64, now: Instant) {
    let elapsed_ms = self
      .maybe_last_threshold_update
      .map_or(0.0, |last| {
        now.saturating_duration_since(last).as_secs_f64() * 1000.0
      })
      .min(100.0);
    self.maybe_last_threshold_update = Some(now);
    if modified_abs > self.threshold_ms + 15.0 {
      return;
    }
    let rate = if modified_abs < self.threshold_ms {
      THRESHOLD_DOWN_RATE
    } else {
      THRESHOLD_UP_RATE
    };
    self.threshold_ms += rate * (modified_abs - self.threshold_ms) * elapsed_ms;
    self.threshold_ms = self.threshold_ms.clamp(THRESHOLD_MIN_MS, THRESHOLD_MAX_MS);
  }
}

impl FeedbackRecorder {
  /// `epoch` is the receiver's time base; the sender gets it once (any shared offset cancels out).
  pub fn new(epoch: Instant) -> Self {
    Self {
      epoch,
      arrivals: BTreeMap::new(),
      maybe_next_to_report: None,
      highest_received: 0,
    }
  }

  pub fn on_arrival(&mut self, seq: TransportSeq, at: Instant) {
    let extended = match self.maybe_next_to_report {
      None => u64::from(seq.0) + (1 << 32),
      Some(_) => unwrap_seq(seq.0, self.highest_received),
    };
    if self.maybe_next_to_report.is_some_and(|next| extended < next) {
      // Reported as lost already.
      return;
    }
    self.maybe_next_to_report.get_or_insert(extended);
    self.highest_received = self.highest_received.max(extended);
    let micros = at.saturating_duration_since(self.epoch).as_micros();
    self
      .arrivals
      .insert(extended, u64::try_from(micros).unwrap_or(u64::MAX));
  }

  /// Feedback for everything since the last call, or `None` when nothing arrived.
  pub fn take_feedback(&mut self) -> Option<TransportFeedback> {
    let start = self.maybe_next_to_report?;
    if self.arrivals.is_empty() {
      return None;
    }
    let end = self.highest_received.min(start + MAX_FEEDBACK_ENTRIES as u64 - 1);
    let arrivals_us = (start..=end).map(|seq| self.arrivals.remove(&seq)).collect();
    self.arrivals.retain(|&seq, _| seq > end);
    self.maybe_next_to_report = Some(end + 1);
    Some(TransportFeedback {
      // Truncation to 16 bits is the wire format.
      base_seq: (start & 0xFFFF) as u16,
      arrivals_us,
    })
  }

  pub fn epoch(&self) -> Instant {
    self.epoch
  }
}

/// Maps a 16-bit sequence number to the 64-bit value closest to `reference`.
fn unwrap_seq(seq: u16, reference: u64) -> u64 {
  let base = reference & !0xFFFF;
  [base.wrapping_sub(0x1_0000), base, base + 0x1_0000]
    .map(|candidate| candidate.wrapping_add(u64::from(seq)))
    .into_iter()
    .min_by_key(|candidate| candidate.abs_diff(reference))
    .unwrap_or(base + u64::from(seq))
}

fn signed_ms(a: Instant, b: Instant) -> f64 {
  if a >= b {
    a.duration_since(b).as_secs_f64() * 1000.0
  } else {
    -(b.duration_since(a).as_secs_f64() * 1000.0)
  }
}

/// Least-squares slope of (x, y) points.
fn linear_slope(points: &VecDeque<(f64, f64)>) -> f64 {
  let count = points.len() as f64;
  let mean_x = points.iter().map(|(x, _)| x).sum::<f64>() / count;
  let mean_y = points.iter().map(|(_, y)| y).sum::<f64>() / count;
  let (numerator, denominator) = points.iter().fold((0.0, 0.0), |(num, den), (x, y)| {
    (num + (x - mean_x) * (y - mean_y), den + (x - mean_x).powi(2))
  });
  if denominator == 0.0 {
    0.0
  } else {
    numerator / denominator
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const PACKET_BYTES: usize = 1200;
  const TICK: Duration = Duration::from_millis(5);
  const FEEDBACK_INTERVAL: Duration = Duration::from_millis(100);

  mod convergence {
    use super::*;

    #[test]
    fn converges_towards_link_capacity() {
      let mut sim = Simulation::new(Link::new(2_000_000, 40, 0));
      sim.run(Duration::from_secs(40));
      let estimate = sim.estimator.estimate_bps();
      assert!((1_400_000..=2_400_000).contains(&estimate), "estimate {estimate}");
      assert!(
        sim.link.queue_delay(sim.now) < Duration::from_millis(250),
        "queue grew unbounded"
      );
    }

    #[test]
    fn high_latency_link_converges_without_a_standing_queue() {
      let mut sim = Simulation::new(Link::new(1_000_000, 150, 0));
      sim.run(Duration::from_secs(40));
      let estimate = sim.estimator.estimate_bps();
      assert!((700_000..=1_200_000).contains(&estimate), "estimate {estimate}");
      assert!(sim.link.queue_delay(sim.now) < Duration::from_millis(150));
    }

    #[test]
    fn capacity_drop_is_followed_quickly() {
      let mut sim = Simulation::new(Link::new(2_000_000, 40, 0));
      sim.run(Duration::from_secs(40));
      sim.link.capacity_bps = 500_000;
      sim.run(Duration::from_secs(3));
      let estimate = sim.estimator.estimate_bps();
      assert!(estimate < 650_000, "estimate {estimate} after the drop");
      sim.run(Duration::from_secs(20));
      let settled = sim.estimator.estimate_bps();
      assert!((350_000..=600_000).contains(&settled), "settled at {settled}");
      assert!(sim.link.queue_delay(sim.now) < Duration::from_millis(100));
    }
  }

  mod loss {
    use super::*;

    #[test]
    fn light_random_loss_does_not_stop_growth() {
      let mut sim = Simulation::new(Link::new(3_000_000, 40, 10));
      sim.run(Duration::from_secs(30));
      assert!(
        sim.estimator.estimate_bps() > 1_000_000,
        "{}",
        sim.estimator.estimate_bps()
      );
    }

    #[test]
    fn heavy_loss_reduces_the_rate() {
      let mut sim = Simulation::new(Link::new(3_000_000, 40, 250));
      sim.run(Duration::from_secs(10));
      assert!(
        sim.estimator.estimate_bps() < 300_000,
        "{}",
        sim.estimator.estimate_bps()
      );
      assert!(sim.estimator.loss_ratio() > 0.15);
    }
  }

  mod recorder {
    use super::*;

    #[test]
    fn reports_arrivals_and_gaps_once() {
      let epoch = Instant::now();
      let mut recorder = FeedbackRecorder::new(epoch);
      recorder.on_arrival(TransportSeq(10), epoch + Duration::from_millis(1));
      recorder.on_arrival(TransportSeq(12), epoch + Duration::from_millis(3));
      let feedback = recorder.take_feedback().unwrap();
      assert_eq!(feedback.base_seq, 10);
      assert_eq!(feedback.arrivals_us, vec![Some(1000), None, Some(3000)]);
      assert_eq!(recorder.take_feedback(), None);
      // 11 arrives late: it was already reported lost.
      recorder.on_arrival(TransportSeq(11), epoch + Duration::from_millis(4));
      assert_eq!(recorder.take_feedback(), None);
    }

    #[test]
    fn sequence_numbers_wrap() {
      let epoch = Instant::now();
      let mut recorder = FeedbackRecorder::new(epoch);
      recorder.on_arrival(TransportSeq(u16::MAX), epoch);
      recorder.on_arrival(TransportSeq(0), epoch);
      let feedback = recorder.take_feedback().unwrap();
      assert_eq!((feedback.base_seq, feedback.arrivals_us.len()), (u16::MAX, 2));
    }

    #[test]
    fn unknown_or_stale_feedback_is_ignored() {
      let now = Instant::now();
      let mut estimator = SendSideEstimator::new(BweConfig::default());
      let feedback = TransportFeedback {
        base_seq: 500,
        arrivals_us: vec![Some(1), None],
      };
      assert_eq!(
        estimator.on_feedback(&feedback, now, now),
        BweConfig::default().start_bps
      );
    }
  }

  /// A bottleneck link: fixed capacity, propagation delay, drop-tail queue and random loss.
  struct Link {
    capacity_bps: u64,
    base_delay: Duration,
    loss_permille: u32,
    queue_limit: Duration,
    maybe_free_at: Option<Instant>,
    rng: XorShift,
  }

  impl Link {
    fn new(capacity_bps: u64, base_delay_ms: u64, loss_permille: u32) -> Self {
      Self {
        capacity_bps,
        base_delay: Duration::from_millis(base_delay_ms),
        loss_permille,
        queue_limit: Duration::from_millis(500),
        maybe_free_at: None,
        rng: XorShift(0x9E37_79B9_7F4A_7C15),
      }
    }

    /// Arrival time at the receiver, or `None` if the packet was dropped.
    fn send(&mut self, bytes: usize, now: Instant) -> Option<Instant> {
      if self.rng.next() % 1000 < u64::from(self.loss_permille) {
        return None;
      }
      if self.queue_delay(now) > self.queue_limit {
        return None;
      }
      let start = self.maybe_free_at.map_or(now, |free| free.max(now));
      let transmission = Duration::from_secs_f64(bytes as f64 * 8.0 / self.capacity_bps as f64);
      self.maybe_free_at = Some(start + transmission);
      Some(start + transmission + self.base_delay)
    }

    fn queue_delay(&self, now: Instant) -> Duration {
      self
        .maybe_free_at
        .map_or(Duration::ZERO, |free| free.saturating_duration_since(now))
    }
  }

  /// Sender pacing at the estimate, link, receiver feedback every 100 ms delivered one way later.
  struct Simulation {
    now: Instant,
    epoch: Instant,
    link: Link,
    estimator: SendSideEstimator,
    recorder: FeedbackRecorder,
    in_flight: VecDeque<(Instant, TransportSeq)>,
    feedback_in_flight: VecDeque<(Instant, TransportFeedback)>,
    next_feedback_at: Instant,
    budget_bytes: f64,
  }

  impl Simulation {
    fn new(link: Link) -> Self {
      let now = Instant::now();
      Self {
        now,
        epoch: now,
        link,
        estimator: SendSideEstimator::new(BweConfig::default()),
        recorder: FeedbackRecorder::new(now),
        in_flight: VecDeque::new(),
        feedback_in_flight: VecDeque::new(),
        next_feedback_at: now + FEEDBACK_INTERVAL,
        budget_bytes: 0.0,
      }
    }

    fn run(&mut self, duration: Duration) {
      let end = self.now + duration;
      while self.now < end {
        self.send_paced();
        self.deliver_packets();
        self.exchange_feedback();
        self.now += TICK;
      }
    }

    fn send_paced(&mut self) {
      self.budget_bytes += f64::from(self.estimator.estimate_bps()) / 8.0 * TICK.as_secs_f64();
      while self.budget_bytes >= PACKET_BYTES as f64 {
        self.budget_bytes -= PACKET_BYTES as f64;
        let seq = self.estimator.on_send(PACKET_BYTES, self.now);
        if let Some(arrival) = self.link.send(PACKET_BYTES, self.now) {
          self.in_flight.push_back((arrival, seq));
        }
      }
    }

    fn deliver_packets(&mut self) {
      while self
        .in_flight
        .front()
        .is_some_and(|(arrival, _)| *arrival <= self.now)
      {
        if let Some((arrival, seq)) = self.in_flight.pop_front() {
          self.recorder.on_arrival(seq, arrival);
        }
      }
    }

    fn exchange_feedback(&mut self) {
      if self.now >= self.next_feedback_at {
        self.next_feedback_at += FEEDBACK_INTERVAL;
        if let Some(feedback) = self.recorder.take_feedback() {
          self
            .feedback_in_flight
            .push_back((self.now + self.link.base_delay, feedback));
        }
      }
      while self
        .feedback_in_flight
        .front()
        .is_some_and(|(at, _)| *at <= self.now)
      {
        if let Some((_, feedback)) = self.feedback_in_flight.pop_front() {
          self.estimator.on_feedback(&feedback, self.now, self.epoch);
        }
      }
    }
  }

  /// Small deterministic PRNG for the link's random loss.
  struct XorShift(u64);

  impl XorShift {
    fn next(&mut self) -> u64 {
      self.0 ^= self.0 << 13;
      self.0 ^= self.0 >> 7;
      self.0 ^= self.0 << 17;
      self.0
    }
  }
}
