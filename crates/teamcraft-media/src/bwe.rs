//! Send-side bandwidth estimation from transport-wide feedback (a simplified Google Congestion
//! Control, draft-ietf-rmcat-gcc).
//!
//! The sender numbers every datagram with a [`TransportSeq`] (datagram header) and remembers when it
//! sent it. The receiver records arrival times ([`FeedbackRecorder`]) and reports them about every
//! 100 ms over the signaling stream. The sender then combines two signals:
//!
//! - **Delay:** if packets arrive increasingly later than they were sent (a queue builds up), a
//!   trendline over the one-way delay variation turns positive. Overuse cuts the rate a step, to
//!   85 % of the estimate; a queue that lasts longer than the window the delivered rate is
//!   measured over, while more is sent than gets through, cuts it to 85 % of what got through,
//!   the path's rate. The queue of a burst lasts as long on a slow path or machine, but nothing
//!   more is sent meanwhile: cut to what got through of it, a screen share's viewer on a CI runner
//!   stayed on the half-size layer. GCC cuts to what got through at once, which assumes a sender
//!   that fills the estimate: a camera's layers and keyframes don't (the queue of a keyframe sent
//!   into a link half used cut a camera's estimate to below its top layer), nor does a still
//!   shared screen that sends little but keyframes of hundreds of kilobytes (#941, #939).
//! - **Loss:** above 10 % loss the rate shrinks with the loss; between 2 % and 10 % it holds. Only
//!   the loss of packets sent after the last cut to what got through counts: that cut answered
//!   the loss before it.
//! - **Application-limited senders** (sending less than half the estimate): the delay of their own
//!   bursts doesn't end the fast start, nor count as congestion (GCC's "ALR"), unless it lasts
//!   with more sent than gets through.
//!
//! Otherwise the rate grows multiplicatively (8 %/s by default; at the start, until the first sign
//! of congestion, much faster: see [`BweConfig::start_increase_per_second`]; and faster again
//! after a while without congestion: see [`BweConfig::recovery_increase_per_second`]), capped at
//! 1.5× the delivered rate so an application-limited sender doesn't drift upwards, unless the sender
//! declares a higher demand ([`SendSideEstimator::set_demand_bps`]): then the estimate may grow up
//! to it, and sending more (e.g. the SFU switching to the next simulcast layer) is the probe. A probe that causes
//! congestion is cut back by the delay detector like any other overuse. The same estimator runs in clients
//! (uplink) and in the SFU per receiver (downlink). Time is passed in; nothing reads a clock.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use teamcraft_proto::TransportFeedback;
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

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
/// Window over which the sent rate tells an application-limited sender.
const APP_LIMITED_WINDOW: Duration = Duration::from_secs(2);
/// A sender that sent less than this share of the estimate over [`APP_LIMITED_WINDOW`] is
/// application-limited.
const APP_LIMITED_SHARE: f64 = 0.5;
/// An overuse that lasts this long is a queue the delivered rate's window ([`ACKED_RATE_WINDOW`])
/// saw all along: what got through is the path's rate, also for an application-limited sender.
const LASTING_OVERUSE: Duration = ACKED_RATE_WINDOW;
/// Sending this much more than gets through is a backlog on the path.
const BACKLOG_SHARE: f64 = 1.1;
/// The longest wait before growing at [`BweConfig::recovery_increase_per_second`] after probes
/// that met congestion.
const MAX_RECOVERY_WAIT: Duration = Duration::from_secs(32);
/// A recovery succeeded once this much more than before the last congestion gets through.
const RECOVERED_SHARE: f64 = 1.1;
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
  /// Growth per second at the start (and after [`SendSideEstimator::measure_afresh`]) until the
  /// first sign of congestion (a decrease, or loss that holds the rate): the start value is a
  /// guess, and what is sent may need far more at once. A viewer of a shared screen waited about
  /// 45 seconds at 8 %/s from 300 kbit/s for the sharp layer and saw the half-size one scaled up
  /// meanwhile (issue #941). Like GCC's start-up probes (3× and 6× the start rate), a path that
  /// can't carry it is cut back by the first overuse or loss.
  pub start_increase_per_second: f64,
  /// Factor of a cut on overuse (of the estimate, or of what got through).
  pub decrease_factor: f64,
  /// Growth per second once the path showed no congestion for [`BweConfig::recovery_after`]
  /// (until the next sign of it): the path may have more room than when it was last congested
  /// (a limit was lifted, cross traffic ended). At 8 %/s, a camera that a 500 kbit/s limit had
  /// cut to its middle layer needed about 20 seconds back to its top layer once the limit was
  /// lifted (#939), and a viewer as long at the SFU. Simulcast layers grow in large steps, so a
  /// sender can't fill the estimate on its way up: the next layer is the probe. A probe that
  /// meets congestion doubles the wait before the next one (up to [`MAX_RECOVERY_WAIT`]), so a
  /// path that stays limited isn't probed every few seconds; once more gets through than before
  /// the congestion, the wait is short again.
  pub recovery_increase_per_second: f64,
  pub recovery_after: Duration,
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
  /// The last cut to (a share of) what got through; a step from the estimate doesn't count.
  maybe_last_delivered_cut_at: Option<Instant>,
  /// Loss counts for packets sent from then on: the last cut to what got through answered the
  /// loss of those sent before. Counted again a second later, the loss of a probe that met a
  /// limit cut the rate a second time, to half of what the path carried (#939).
  maybe_loss_counts_from: Option<Instant>,
  /// (feedback time, lost, reported) of the last [`LOSS_WINDOW`].
  loss_history: VecDeque<(Instant, usize, usize)>,
  last_loss_ratio: f64,
  maybe_demand_bps: Option<u32>,
  /// After a delay-based decrease, no further one until the detector reported normal again (the
  /// queue needs time to drain; cutting on every report would spiral down).
  is_holding_after_decrease: bool,
  /// No sign of congestion since the start or [`SendSideEstimator::measure_afresh`]: growth at
  /// [`BweConfig::start_increase_per_second`].
  is_starting: bool,
  /// The last sign of congestion (a queue building up, loss), for
  /// [`BweConfig::recovery_increase_per_second`].
  maybe_last_congestion_at: Option<Instant>,
  /// What got through at the last sign of congestion.
  congested_at_bps: f64,
  /// How long the path has to show no congestion before the estimate grows faster.
  recovery_wait: Duration,
  /// The estimate grew at [`BweConfig::recovery_increase_per_second`] since the last congestion:
  /// the next congestion is a probe that failed.
  has_grown_recovering: bool,
  /// Since when the delay detector reports overuse, without a break.
  maybe_overuse_since: Option<Instant>,
  /// The current overuse already cut the rate to what got through ([`LASTING_OVERUSE`]).
  has_cut_to_delivered: bool,
  /// (send time, bytes) of the last [`APP_LIMITED_WINDOW`], and their sum.
  recent_sends: VecDeque<(Instant, usize)>,
  recent_sent_bytes: usize,
  maybe_first_send_at: Option<Instant>,
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
      // Triples every second: 300 kbit/s reach a 1440p screen share's 8 Mbit/s in three seconds.
      start_increase_per_second: 2.0,
      decrease_factor: 0.85,
      // Slow enough that a probe of the next step (a whole layer more) shows congestion before
      // the estimate reached the step after it; from a 500 kbit/s limit, a camera's 2 Mbit/s
      // are about three and a half seconds after the wait.
      recovery_increase_per_second: 0.5,
      recovery_after: Duration::from_secs(2),
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
      maybe_last_delivered_cut_at: None,
      maybe_loss_counts_from: None,
      loss_history: VecDeque::new(),
      last_loss_ratio: 0.0,
      maybe_demand_bps: None,
      is_holding_after_decrease: false,
      is_starting: true,
      maybe_last_congestion_at: None,
      congested_at_bps: 0.0,
      recovery_wait: config.recovery_after,
      has_grown_recovering: false,
      maybe_overuse_since: None,
      has_cut_to_delivered: false,
      recent_sends: VecDeque::new(),
      recent_sent_bytes: 0,
      maybe_first_send_at: None,
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
    self.maybe_first_send_at.get_or_insert(now);
    self.recent_sends.push_back((now, bytes));
    self.recent_sent_bytes += bytes;
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
      // Loss of packets sent before the last cut to what got through was answered by it.
      let is_counted = self.maybe_loss_counts_from.is_none_or(|from| sent.at >= from);
      reported += usize::from(is_counted);
      match maybe_arrival_us {
        Some(arrival_us) => {
          let arrival = receiver_epoch + Duration::from_micros(*arrival_us);
          self.delay.on_packet(sent.at, arrival, now);
          self.acked.push_back((arrival, sent.bytes));
        },
        None => lost += usize::from(is_counted),
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

  /// The sender starts sending far more than so far (a camera in a call that sent only sound):
  /// what the estimate learned from the smaller traffic says little about the path. It grew only
  /// as far as that traffic let it, and one lossy or jittery moment cut it to what the sound got
  /// through, far below any video (#935). Measured afresh, as at the start: the estimate is at
  /// least the start value again, and the loss, delay and delivered rate seen so far are
  /// forgotten, and the estimate grows as fast as at the start until the path shows congestion.
  /// Packets in flight still meet their feedback.
  pub fn measure_afresh(&mut self) {
    self.estimate_bps = self
      .estimate_bps
      .max(f64::from(self.config.start_bps))
      .clamp(f64::from(self.config.min_bps), f64::from(self.config.max_bps));
    self.delay = DelayDetector::new();
    self.acked.clear();
    self.loss_history.clear();
    self.last_loss_ratio = 0.0;
    self.maybe_last_update_at = None;
    self.maybe_last_decrease_at = None;
    self.maybe_last_delivered_cut_at = None;
    self.maybe_loss_counts_from = None;
    self.is_holding_after_decrease = false;
    self.is_starting = true;
    self.maybe_last_congestion_at = None;
    self.congested_at_bps = 0.0;
    self.recovery_wait = self.config.recovery_after;
    self.has_grown_recovering = false;
    self.maybe_overuse_since = None;
    self.has_cut_to_delivered = false;
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
    let sent_bps = self.recent_sent_bps(now);
    let is_app_limited = sent_bps < APP_LIMITED_SHARE * self.estimate_bps;
    // Congestion (a queue building up, loss that holds or cuts the rate) ends the start; the
    // queue of an application-limited sender's own burst doesn't.
    let is_overusing = self.delay.usage == BandwidthUsage::Overusing;
    let overuse_since = if is_overusing {
      Some(*self.maybe_overuse_since.get_or_insert(now))
    } else {
      self.has_cut_to_delivered = false;
      None
    };
    self.maybe_overuse_since = overuse_since;
    // A queue that lasts while less gets through than is sent: what gets through is the path's
    // rate. The queue of a burst (a still screen's keyframe on a slow machine) can last as long,
    // but what got through of it is more than the sender's rate around it: the path carried it.
    let is_overuse_lasting = self.sent_bps_within(now, ACKED_RATE_WINDOW) > BACKLOG_SHARE * acked_bps
      && overuse_since.is_some_and(|since| now.saturating_duration_since(since) >= LASTING_OVERUSE);
    let is_recovering = self
      .maybe_last_congestion_at
      .is_some_and(|last| now.saturating_duration_since(last) >= self.recovery_wait);
    if acked_bps > RECOVERED_SHARE * self.congested_at_bps {
      self.recovery_wait = self.config.recovery_after;
    }
    if (is_overusing && (!is_app_limited || is_overuse_lasting)) || self.last_loss_ratio >= 0.02 {
      self.is_starting = false;
      self.note_congestion(now, acked_bps);
    }
    match self.delay.usage {
      BandwidthUsage::Overusing => {
        if is_overuse_lasting
          && !self.has_cut_to_delivered
          && acked_bps > 0.0
          && self.may_decrease(now, DELAY_DECREASE_INTERVAL)
        {
          // The queue outlasted the delivered rate's window: what got through is the path's
          // rate, whatever the sender planned.
          self.estimate_bps = self.estimate_bps.min(self.config.decrease_factor * acked_bps);
          self.maybe_last_decrease_at = Some(now);
          self.note_cut_to_delivered(now);
          self.is_holding_after_decrease = true;
          self.has_cut_to_delivered = true;
        } else if acked_bps > 0.0
          && !self.is_holding_after_decrease
          && self.may_decrease(now, DELAY_DECREASE_INTERVAL)
        {
          // A step: what got through says little while the sender didn't fill the estimate (the
          // queue of a keyframe sent into a link half used cut a camera below its top layer); a
          // backlog that stays is cut to it above.
          self.estimate_bps *= self.config.decrease_factor;
          self.maybe_last_decrease_at = Some(now);
          self.is_holding_after_decrease = true;
        }
      },
      BandwidthUsage::Underusing => {},
      BandwidthUsage::Normal => {
        self.is_holding_after_decrease = false;
        if self.last_loss_ratio < 0.02 {
          let increase = if self.is_starting {
            self.config.start_increase_per_second
          } else if is_recovering {
            self.has_grown_recovering = true;
            self.config.recovery_increase_per_second
          } else {
            self.config.increase_per_second
          };
          let growth = (1.0 + increase).powf(elapsed.as_secs_f64());
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
    // Timed apart from other cuts to what got through (they answer the same congestion, and
    // what gets through shrinks after them), but not from a step down from the estimate: that
    // held the loss cut back for a second (#939).
    let may_cut_for_loss = self
      .maybe_last_delivered_cut_at
      .is_none_or(|last| now.saturating_duration_since(last) >= LOSS_WINDOW);
    if self.last_loss_ratio > 0.10 && acked_bps > 0.0 && may_cut_for_loss {
      // Based on what got through, not on the previous estimate, so repeated reports of the same
      // loss episode don't compound. Loss is damage also to an application-limited sender.
      let loss_based = acked_bps * (1.0 - 0.5 * self.last_loss_ratio);
      self.estimate_bps = self.estimate_bps.min(loss_based);
      self.maybe_last_decrease_at = Some(now);
      self.note_cut_to_delivered(now);
    }
    self.estimate_bps = self
      .estimate_bps
      .clamp(f64::from(self.config.min_bps), f64::from(self.config.max_bps));
  }

  /// The rate was cut to (a share of) what got through at `now`.
  fn note_cut_to_delivered(&mut self, now: Instant) {
    self.maybe_last_delivered_cut_at = Some(now);
    self.maybe_loss_counts_from = Some(now);
    self.loss_history.clear();
    self.last_loss_ratio = 0.0;
  }

  /// A sign of congestion at `now` with `acked_bps` getting through; after growth at the
  /// recovery rate, the probe failed and the next one waits twice as long.
  fn note_congestion(&mut self, now: Instant, acked_bps: f64) {
    if std::mem::take(&mut self.has_grown_recovering) {
      self.recovery_wait = (self.recovery_wait * 2).min(MAX_RECOVERY_WAIT);
    }
    self.maybe_last_congestion_at = Some(now);
    self.congested_at_bps = acked_bps;
  }

  /// What the sender sent over the last `window` (at most [`APP_LIMITED_WINDOW`]), or since its
  /// first packet when that is more recent.
  fn sent_bps_within(&self, now: Instant, window: Duration) -> f64 {
    let Some(first_send_at) = self.maybe_first_send_at else {
      return 0.0;
    };
    let bytes: usize = self
      .recent_sends
      .iter()
      .rev()
      .take_while(|(at, _)| now.saturating_duration_since(*at) <= window)
      .map(|(_, bytes)| bytes)
      .sum();
    let span = now
      .saturating_duration_since(first_send_at)
      .clamp(Duration::from_millis(100), window);
    bytes as f64 * 8.0 / span.as_secs_f64()
  }

  /// What the sender sent lately (over [`APP_LIMITED_WINDOW`], or since its first packet when that
  /// is more recent); below [`APP_LIMITED_SHARE`] of the estimate it is application-limited.
  fn recent_sent_bps(&mut self, now: Instant) -> f64 {
    while let Some(&(at, bytes)) = self.recent_sends.front() {
      if now.saturating_duration_since(at) <= APP_LIMITED_WINDOW {
        break;
      }
      self.recent_sends.pop_front();
      self.recent_sent_bytes -= bytes;
    }
    let Some(first_send_at) = self.maybe_first_send_at else {
      return 0.0;
    };
    let span = now
      .saturating_duration_since(first_send_at)
      .clamp(Duration::from_millis(200), APP_LIMITED_WINDOW);
    self.recent_sent_bytes as f64 * 8.0 / span.as_secs_f64()
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

    /// Issue #941: from the start value, a viewer of a 1440p screen share needs about 8 Mbit/s.
    /// At 8 %/s that took 45 seconds; the start triples per second until the path shows
    /// congestion, then the estimate settles at the capacity as before.
    #[test]
    fn the_start_grows_fast_until_the_path_shows_congestion() {
      let mut sim = Simulation::new(Link::new(12_000_000, 40, 0));
      sim.run(Duration::from_secs(4));
      let estimate = sim.estimator.estimate_bps();
      assert!(estimate >= 8_000_000, "estimate {estimate} after four seconds");
      sim.run(Duration::from_secs(20));
      let settled = sim.estimator.estimate_bps();
      assert!(
        (8_000_000..=13_000_000).contains(&settled),
        "settled at {settled}"
      );
      assert!(sim.link.queue_delay(sim.now) < Duration::from_millis(250));
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

  mod recovery {
    use super::*;

    /// Issue #939: a limit lifted from a path is found within seconds, not at 8 %/s (about 20
    /// seconds from 500 kbit/s to 2 Mbit/s).
    #[test]
    fn a_lifted_limit_is_found_within_seconds() {
      let mut sim = Simulation::new(Link::new(500_000, 20, 0));
      sim.run(Duration::from_secs(20));
      assert!(sim.estimator.estimate_bps() < 650_000);
      sim.link.capacity_bps = 5_000_000;
      sim.run(Duration::from_secs(8));
      let estimate = sim.estimator.estimate_bps();
      assert!(
        estimate >= 2_000_000,
        "estimate {estimate} eight seconds after the limit"
      );
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

    /// Issue #935: about to send far more (a camera in a call that sent only sound), the estimate
    /// starts from the start value again, and the loss seen so far no longer counts.
    #[test]
    fn measuring_afresh_starts_from_the_start_value_without_the_old_loss() {
      let mut sim = Simulation::new(Link::new(3_000_000, 40, 250));
      sim.run(Duration::from_secs(10));
      assert!(sim.estimator.estimate_bps() < 300_000);
      sim.estimator.measure_afresh();
      assert_eq!(sim.estimator.estimate_bps(), BweConfig::default().start_bps);
      assert!(sim.estimator.loss_ratio() < f64::EPSILON);
      // A higher estimate stays.
      let mut sim = Simulation::new(Link::new(3_000_000, 40, 0));
      sim.run(Duration::from_secs(30));
      let before = sim.estimator.estimate_bps();
      assert!(before > BweConfig::default().start_bps);
      sim.estimator.measure_afresh();
      assert_eq!(sim.estimator.estimate_bps(), before);
    }
  }

  mod app_limited {
    use super::*;

    /// Issue #939: a camera sends 1.2 Mbit/s while its estimate grew to its demand of 2 Mbit/s,
    /// and its path drops to 500 kbit/s. A step of 15 % at a time held the picture up for
    /// seconds; the queue that stays cuts the estimate to what the path carries.
    #[test]
    fn a_queue_that_stays_cuts_to_what_gets_through() {
      let mut sim = Simulation::new(Link::new(10_000_000, 20, 0));
      sim.estimator.set_demand_bps(Some(2_000_000));
      sim.sending = Sending::Fixed(1_200_000);
      sim.run(Duration::from_secs(5));
      assert!(sim.estimator.estimate_bps() >= 1_900_000);
      sim.link.capacity_bps = 500_000;
      sim.run(Duration::from_millis(1_200));
      let estimate = sim.estimator.estimate_bps();
      assert!(estimate <= 500_000, "estimate {estimate} 1.2 s after the drop");
    }

    /// A still screen's keyframe (360 KB) on a slow path, or read by a slow machine (a CI
    /// runner), queues for seconds. What got through of it is no less than what was sent
    /// meanwhile: no backlog, so no cut to it, and the fast start goes on. Cut to what the burst
    /// delivered, a screen share's viewer stayed on the half-size layer (#939).
    #[test]
    fn a_long_burst_without_a_backlog_is_not_cut_to_what_got_through() {
      let config = BweConfig {
        start_bps: 8_000_000,
        ..BweConfig::default()
      };
      let mut estimator = SendSideEstimator::new(config);
      estimator.set_demand_bps(Some(9_000_000));
      let epoch = Instant::now();
      // 300 packets sent within 300 ms, arriving one every 10 ms over three seconds; then a
      // packet every 100 ms. Feedback every 100 ms.
      let mut sent = Vec::new();
      for index in 0..300u64 {
        estimator.on_send(PACKET_BYTES, epoch + Duration::from_millis(index));
        sent.push((index, 20_000 + index * 10_000));
      }
      let mut next_seq = 300u64;
      let mut reported = 0usize;
      for tick in 1..=40u64 {
        let now = epoch + Duration::from_millis(100 * tick);
        if tick > 3 {
          estimator.on_send(PACKET_BYTES, now);
          sent.push((next_seq, (100_000 * tick + 20_000).max(3_020_000 + next_seq)));
          next_seq += 1;
        }
        let mut due: Vec<(u64, u64)> = sent
          .iter()
          .copied()
          .skip(reported)
          .filter(|(_, arrival_us)| *arrival_us <= 100_000 * tick)
          .collect();
        due.sort_unstable();
        // Reported in order of sequence numbers, up to the first that hasn't arrived.
        let first = reported as u64;
        let count = due
          .iter()
          .enumerate()
          .take_while(|(offset, (seq, _))| *seq == first + *offset as u64)
          .count();
        if count == 0 {
          continue;
        }
        let feedback = TransportFeedback {
          base_seq: first as u16,
          arrivals_us: due[..count]
            .iter()
            .map(|(_, arrival_us)| Some(*arrival_us))
            .collect(),
        };
        reported += count;
        estimator.on_feedback(&feedback, now, epoch);
      }
      let estimate = estimator.estimate_bps();
      assert!(
        estimate >= 6_000_000,
        "cut to what the burst delivered: {estimate}"
      );
      assert!(estimator.is_starting, "the burst ends the fast start");
    }

    /// Issue #941: a still screen share sends little but bursts of keyframes, and the queue of a
    /// burst reads as overuse. What got through is far below the path's rate then: the cut
    /// starts from the estimate, not from it, and the fast start goes on.
    #[test]
    fn the_queue_of_an_application_limited_burst_cuts_the_estimate_a_step() {
      let config = BweConfig {
        start_bps: 8_000_000,
        ..BweConfig::default()
      };
      let mut estimator = SendSideEstimator::new(config);
      estimator.set_demand_bps(Some(9_000_000));
      let epoch = Instant::now();
      // One packet every 10 ms (about 1 Mbit/s), each waiting 3 ms longer than the one before.
      let mut feedback_base = 0u16;
      let mut arrivals_us = Vec::new();
      for index in 0..60u64 {
        let sent_at = epoch + Duration::from_millis(10 * index);
        estimator.on_send(PACKET_BYTES, sent_at);
        arrivals_us.push(Some((10 * index + 20 + 3 * index) * 1000));
        if arrivals_us.len() == 10 {
          let feedback = TransportFeedback {
            base_seq: feedback_base,
            arrivals_us: std::mem::take(&mut arrivals_us),
          };
          feedback_base += 10;
          estimator.on_feedback(&feedback, sent_at + Duration::from_millis(30), epoch);
        }
      }
      assert_eq!(estimator.usage(), BandwidthUsage::Overusing);
      let estimate = estimator.estimate_bps();
      assert!(
        (6_000_000..8_000_000).contains(&estimate),
        "one step down from the estimate, not to what got through: {estimate}"
      );
      assert!(estimator.is_starting, "the burst ends the fast start");
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
    sending: Sending,
  }

  /// What the simulated sender sends.
  enum Sending {
    /// Paced at the estimate.
    Estimate,
    /// A camera's encoders: a fixed rate, whatever the estimate.
    Fixed(u32),
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
        sending: Sending::Estimate,
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
      self.budget_bytes += f64::from(self.sending_bps()) / 8.0 * TICK.as_secs_f64();
      while self.budget_bytes >= PACKET_BYTES as f64 {
        self.budget_bytes -= PACKET_BYTES as f64;
        let seq = self.estimator.on_send(PACKET_BYTES, self.now);
        if let Some(arrival) = self.link.send(PACKET_BYTES, self.now) {
          self.in_flight.push_back((arrival, seq));
        }
      }
    }

    fn sending_bps(&self) -> u32 {
      match self.sending {
        Sending::Estimate => self.estimator.estimate_bps(),
        Sending::Fixed(bps) => bps,
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
