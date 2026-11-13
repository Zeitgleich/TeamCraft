//! Probing with padding whether a link carries a rate (#994, #1792): the SFU before switching a
//! viewer up a layer, the client while its uplink holds layers back.
//!
//! An estimate grows only while the sender fills it, and a layered sender can't: its next layer is
//! a step of hundreds of kilobits or megabits. Sending that layer to find out whether it fits
//! stalled the picture of a viewer whose link couldn't carry it (#994), and an estimate that waits
//! for growth after congestion followed a link on the motorway minutes late (#1792). So the link
//! is probed with padding first, as WebRTC probes while a sender is application-limited: a short
//! cluster of padding trains on [`crate::datagram::PADDING_STREAM`], numbered for the receiver's
//! transport feedback like any datagram, sized so that what is sent anyway plus the padding is the
//! rate in question for the cluster's length.
//!
//! **What tells the answer is the queue, not the spread.** Each train is sent back to back, a timer
//! tick ([`RateProbeConfig::train_spacing`]) after the previous one. A link that carries the rate
//! drains each train before the next one comes; one that doesn't queues the excess, and every
//! train waits longer than the one before. The probe compares the least one-way delay of the
//! first and the last train ([`RateProbeConfig::max_queue_growth`]); clock offsets cancel out. The
//! spread of a single burst (packet-train dispersion) shows the bottleneck's line rate instead, and
//! a token bucket in front of it (Linux `tbf`, an LTE cell's grant) passes a burst of tens of
//! kilobytes at once: a 3 Mbit/s link with a 32 kB bucket showed five times its rate. Within a
//! bucket that size a cluster still can't tell a rate a little above the link's from one below
//! it; the estimate's delay detector catches that once the layer flows.
//!
//! - **Bounded:** one cluster at a time, [`RateProbeConfig::trains`] trains of at most
//!   [`RateProbeConfig::max_datagrams`]; a cluster that fell short delays the next by
//!   [`RateProbeConfig::retry_after`], doubling up to [`RateProbeConfig::max_retry_after`] (1 s,
//!   then 2 s: on a link that swings room comes and goes within seconds, and a cluster that fell
//!   in a dip right before room appeared held the top layer more than 5 s after it at 2 to 4 s,
//!   #1792); one that got through ends that.
//!   The smaller step after a failure ([`RateProbeConfig::step_factors`]) is the care instead.
//! - **In steps:** a cluster probes at most [`RateProbeConfig::step_factors`] times what the link
//!   carries now ([`RateProbe::target_bps`]): 1.5 times, 1.25 after clusters that fell short, 2
//!   after two in a row that got through. One
//!   that fell short put its excess into the link's queue; a 1440p share's top layer probed from a
//!   2 Mbit/s link overflowed a mobile link's buffer and took video with it. A link with room is
//!   climbed a step per cluster, about every 400 ms.
//! - **Lost padding** means the link's queue overflowed: short of the rate, whatever the delays.
//! - **Nothing to learn:** padding carries zeros; the receiver sees only how much arrived when.

use std::time::Duration;

use teamcraft_proto::TransportFeedback;
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::bwe::SendSideEstimator;
use crate::datagram::{TransportSeq, padding_datagram};

/// Settings of the probes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateProbeConfig {
  /// Trains in a cluster: the first and the last are compared.
  pub trains: u8,
  /// The time between two trains; each carries the padding of this long. The callers' timers run
  /// about this often (100 ms), and a train that comes later carries more.
  pub train_spacing: Duration,
  /// Bytes per padding datagram: large enough that QUIC sends each in a packet of its own.
  pub datagram_bytes: usize,
  /// The smallest padding datagram: a train whose padding is less than `min_datagrams` of
  /// `datagram_bytes` sends `min_datagrams` this small or larger instead (#1893).
  pub min_datagram_bytes: usize,
  /// Datagrams per train, at least (the least delay of a train needs a few) and at most.
  pub min_datagrams: usize,
  pub max_datagrams: usize,
  /// The last train may wait at most this much longer than the first for the link to carry the
  /// rate: a link short by an eighth queues about 25 ms over a cluster of 200 ms, and a mobile
  /// link's jitter stays below it at the least delay of a train.
  pub max_queue_growth: Duration,
  /// How long what a cluster showed counts.
  pub result_valid_for: Duration,
  /// The wait after a cluster that fell short before the next one; it doubles with every further
  /// one up to `max_retry_after` (by default the same), and one that gets through ends it.
  pub retry_after: Duration,
  pub max_retry_after: Duration,
  /// A cluster whose feedback hasn't come within this after its last train is given up.
  pub timeout: Duration,
  /// A cluster probes at most this many times what the link carries now: after one or more that
  /// fell short, at first (or after one that got through), and after two or more in a row that
  /// got through. A cluster that falls short queues its excess for about 300 ms: at half again, a
  /// buffer of 150 ms overflows only where the link already queued; a link that carried two steps
  /// in a row has room, and is climbed faster.
  pub step_factors: [f64; 3],
}

/// The probes of one link.
#[derive(Debug, Default)]
pub struct RateProbe {
  maybe_cluster: Option<Cluster>,
  /// The rate the last cluster showed the link carries, and when.
  maybe_shown: Option<(u32, Instant)>,
  maybe_next_at: Option<Instant>,
  /// The wait after the next cluster that falls short; zero before the first.
  retry_wait: Duration,
  /// Clusters that fell short since the last that got through.
  failures: usize,
  /// Clusters in a row that got through.
  successes: usize,
}

/// A sender's probes of its own uplink while it holds layers back (#1792): the client's side of
/// [`RateProbe`], the SFU probes each receiver's downlink the same way. A cluster that got through
/// lifts the estimate to its rate at once, and the layers follow with the next feedback; one that
/// fell short holds the estimate where it is, so the cluster's own queue cuts nothing.
#[derive(Debug, Default)]
pub struct UplinkProbe {
  probe: RateProbe,
}

/// What a cluster showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
  /// The link carried `target_bps`.
  Carried { target_bps: u32 },
  /// The link carries less than the target. `floor_bps` keeps what the receiver gets now.
  FellShort { target_bps: u32, floor_bps: u32 },
}

/// A cluster in flight.
#[derive(Debug)]
struct Cluster {
  target_bps: u32,
  floor_bps: u32,
  /// The padding's rate on top of what is sent anyway.
  padding_bps: u32,
  started_at: Instant,
  maybe_last_train_at: Option<Instant>,
  trains: Vec<Train>,
}

#[derive(Debug)]
struct Train {
  first_seq: TransportSeq,
  sent_at: Instant,
  /// Per datagram: its bytes, and what the feedback said of it.
  datagrams: Vec<Report>,
}

/// What the receiver's feedback said of one datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
  Pending,
  Lost,
  /// Arrival time in µs (receiver's clock).
  Arrived(u64),
}

impl Default for RateProbeConfig {
  fn default() -> Self {
    Self {
      trains: 3,
      train_spacing: Duration::from_millis(100),
      datagram_bytes: 1000,
      min_datagram_bytes: 100,
      min_datagrams: 2,
      max_datagrams: 100,
      max_queue_growth: Duration::from_millis(25),
      result_valid_for: Duration::from_secs(3),
      retry_after: Duration::from_secs(1),
      max_retry_after: Duration::from_secs(2),
      timeout: Duration::from_secs(1),
      step_factors: [1.25, 1.5, 2.0],
    }
  }
}

impl RateProbeConfig {
  /// A voice below its full step probing for the next one (#1893): the link is a few tens of
  /// kilobits, and each cluster that falls short costs the voice that waits behind its padding.
  /// One every second or two kept a 64 kbit/s uplink too full for the voice it had; a link just
  /// too thin for the next step is tried again after seconds, then every half minute.
  pub fn for_voice() -> Self {
    Self {
      retry_after: Duration::from_secs(5),
      max_retry_after: Duration::from_secs(40),
      ..Self::default()
    }
  }
}

impl RateProbe {
  pub fn new() -> Self {
    Self::default()
  }

  /// The rate a recent cluster showed the link carries; 0 without one.
  pub fn shown_bps(&self, config: &RateProbeConfig, now: Instant) -> u32 {
    self
      .maybe_shown
      .filter(|(_, at)| now.saturating_duration_since(*at) <= config.result_valid_for)
      .map_or(0, |(bps, _)| bps)
  }

  /// Another measurement (the probe at the start, `crate::join_probe`) showed the link carries
  /// `bps` at `now`: it counts as a cluster's result.
  pub fn note_shown(&mut self, bps: u32, now: Instant) {
    self.maybe_shown = Some((bps, now));
  }

  /// The rate the next cluster probes when the link has to carry `wanted_bps` and is known to
  /// carry `carried_bps` (what gets through now, at most the estimate): at most a step above that
  /// ([`RateProbeConfig::step_factors`]).
  pub fn target_bps(&self, config: &RateProbeConfig, wanted_bps: u32, carried_bps: u32) -> u32 {
    let [after_failure, first, after_successes] = config.step_factors;
    let factor = if self.failures > 0 {
      after_failure
    } else if self.successes >= 2 {
      after_successes
    } else {
      first
    };
    // Saturating float-to-int conversion is intended.
    wanted_bps.min((f64::from(carried_bps) * factor) as u32)
  }

  /// Whether a cluster may start now: none in flight, and the last one long enough ago.
  pub fn may_start(&self, now: Instant) -> bool {
    self.maybe_cluster.is_none() && self.maybe_next_at.is_none_or(|next| now >= next)
  }

  /// Whether a cluster is in flight.
  pub fn is_running(&self) -> bool {
    self.maybe_cluster.is_some()
  }

  /// Starts a cluster at `now` that probes whether the link carries `target_bps` while
  /// `sending_bps` is sent anyway; `floor_bps` is what a cluster that falls short keeps. Its
  /// first train is due at once ([`Self::due_train`]).
  pub fn start(&mut self, target_bps: u32, floor_bps: u32, sending_bps: u32, now: Instant) {
    // At least an eighth of the target as padding: what is sent anyway comes in frames, and a
    // cluster of a few datagrams says little.
    let padding_bps = target_bps.saturating_sub(sending_bps).max(target_bps / 8);
    self.maybe_cluster = Some(Cluster {
      target_bps,
      floor_bps,
      padding_bps,
      started_at: now,
      maybe_last_train_at: None,
      trains: Vec::new(),
    });
  }

  /// The datagram sizes of the cluster's next train, when one is due at `now`.
  pub fn due_train(&self, config: &RateProbeConfig, now: Instant) -> Option<Vec<usize>> {
    let cluster = self.maybe_cluster.as_ref()?;
    if cluster.trains.len() >= usize::from(config.trains) {
      return None;
    }
    let elapsed = match cluster.maybe_last_train_at {
      None => config.train_spacing,
      // A timer a little early still sends the train (a train waiting for the next tick doubles
      // the cluster's length, and the queue a link that falls short builds); a late one carries
      // the padding of the time since, within reason.
      Some(last) if now.saturating_duration_since(last) >= config.train_spacing * 3 / 4 => {
        now.saturating_duration_since(last).min(config.train_spacing * 2)
      },
      Some(_) => return None,
    };
    let bytes = f64::from(cluster.padding_bps) / 8.0 * elapsed.as_secs_f64();
    // Saturating float-to-int conversion is intended.
    let count = ((bytes / config.datagram_bytes as f64).ceil() as usize)
      .clamp(config.min_datagrams, config.max_datagrams);
    // A thin link's train is its padding in smaller datagrams, not `min_datagrams` whole ones:
    // two of 1000 bytes every 100 ms are 160 kbit/s, and probing a 64 kbit/s link for a voice's
    // next step flooded it for half a second with every cluster (#1893).
    let size =
      ((bytes / count as f64).ceil() as usize).clamp(config.min_datagram_bytes, config.datagram_bytes);
    Some(vec![size; count])
  }

  /// A train of `sizes` went out at `now`, numbered from `first_seq` on.
  pub fn sent(&mut self, first_seq: TransportSeq, sizes: &[usize], now: Instant) {
    let Some(cluster) = self.maybe_cluster.as_mut() else {
      return;
    };
    cluster.maybe_last_train_at = Some(now);
    cluster.trains.push(Train {
      first_seq,
      sent_at: now,
      datagrams: vec![Report::Pending; sizes.len()],
    });
  }

  /// Notes the arrivals of the cluster's datagrams; once all are reported, what it showed.
  pub fn on_feedback(
    &mut self,
    config: &RateProbeConfig,
    feedback: &TransportFeedback,
    now: Instant,
  ) -> Option<ProbeOutcome> {
    let cluster = self.maybe_cluster.as_mut()?;
    for train in &mut cluster.trains {
      for (offset, maybe_arrival_us) in feedback.arrivals_us.iter().enumerate() {
        // Feedback numbers are 16 bits wide like the sequence numbers; a train is a few of them.
        let seq = feedback.base_seq.wrapping_add(offset as u16);
        let index = usize::from(seq.wrapping_sub(train.first_seq.0));
        if let Some(report) = train.datagrams.get_mut(index) {
          *report = maybe_arrival_us.map_or(Report::Lost, Report::Arrived);
        }
      }
    }
    let is_reported = cluster.trains.len() == usize::from(config.trains)
      && cluster
        .trains
        .iter()
        .all(|train| train.datagrams.iter().all(|report| *report != Report::Pending));
    if !is_reported {
      return None;
    }
    let outcome = cluster.outcome(config);
    self.finish(config, outcome, now);
    Some(outcome)
  }

  /// Gives up a cluster whose feedback doesn't come (the receiver lost it, or reports none).
  pub fn on_timer(&mut self, config: &RateProbeConfig, now: Instant) {
    let is_overdue = self.maybe_cluster.as_ref().is_some_and(|cluster| {
      let last = cluster.maybe_last_train_at.unwrap_or(cluster.started_at);
      now.saturating_duration_since(last) >= config.timeout
    });
    if is_overdue {
      self.maybe_cluster = None;
      self.maybe_next_at = Some(now + config.retry_after);
    }
  }

  fn finish(&mut self, config: &RateProbeConfig, outcome: ProbeOutcome, now: Instant) {
    self.maybe_cluster = None;
    match outcome {
      ProbeOutcome::Carried { target_bps } => {
        self.maybe_shown = Some((target_bps, now));
        self.maybe_next_at = None;
        self.retry_wait = Duration::ZERO;
        self.failures = 0;
        self.successes += 1;
      },
      ProbeOutcome::FellShort { .. } => {
        // What an earlier cluster showed is in doubt: a full token bucket in front of the link
        // passes the excess of a short cluster unseen, and the next step taken from there
        // overflowed its buffer.
        self.maybe_shown = None;
        self.failures += 1;
        self.successes = 0;
        self.retry_wait = (self.retry_wait * 2).clamp(config.retry_after, config.max_retry_after);
        self.maybe_next_at = Some(now + self.retry_wait);
      },
    }
  }
}

impl UplinkProbe {
  pub fn new() -> Self {
    Self::default()
  }

  /// The padding datagrams to send at `now`, numbered by `uplink`. `maybe_wanted_bps` is what
  /// sending everything takes while layers are held back (`None` while nothing is): a cluster
  /// starts when the estimate is below it, outside the estimate's fast start (it grows on its own
  /// there), and steps from what gets through now.
  pub fn on_timer(
    &mut self,
    config: &RateProbeConfig,
    uplink: &mut SendSideEstimator,
    maybe_wanted_bps: Option<u32>,
    now: Instant,
  ) -> Vec<Vec<u8>> {
    self.probe.on_timer(config, now);
    if !self.probe.is_running() && !self.start_if_due(config, uplink, maybe_wanted_bps, now) {
      return Vec::new();
    }
    let Some(sizes) = self.probe.due_train(config, now) else {
      return Vec::new();
    };
    let mut maybe_first = None;
    let datagrams = sizes
      .iter()
      .map(|bytes| {
        let seq = uplink.on_send_probe(*bytes, now);
        maybe_first.get_or_insert(seq);
        padding_datagram(*bytes, seq)
      })
      .collect();
    if let Some(first) = maybe_first {
      self.probe.sent(first, &sizes, now);
    }
    datagrams
  }

  /// Notes `feedback` and answers a cluster it completes on `uplink`.
  pub fn on_feedback(
    &mut self,
    config: &RateProbeConfig,
    feedback: &TransportFeedback,
    uplink: &mut SendSideEstimator,
    now: Instant,
  ) -> Option<ProbeOutcome> {
    let outcome = self.probe.on_feedback(config, feedback, now)?;
    match outcome {
      ProbeOutcome::Carried { target_bps } => uplink.raise_to(target_bps),
      ProbeOutcome::FellShort {
        target_bps,
        floor_bps,
      } => {
        uplink.after_short_probe(target_bps, floor_bps, now);
      },
    }
    Some(outcome)
  }

  fn start_if_due(
    &mut self,
    config: &RateProbeConfig,
    uplink: &SendSideEstimator,
    maybe_wanted_bps: Option<u32>,
    now: Instant,
  ) -> bool {
    let Some(wanted_bps) = maybe_wanted_bps else {
      return false;
    };
    let estimate_bps = uplink.estimate_bps();
    if estimate_bps >= wanted_bps || uplink.is_in_fast_start() || !self.probe.may_start(now) {
      return false;
    }
    let sending_bps = uplink.sent_bps(now);
    let carried_bps = estimate_bps.min(sending_bps.max(self.probe.shown_bps(config, now)));
    let target_bps = self.probe.target_bps(config, wanted_bps, carried_bps);
    if target_bps <= carried_bps {
      return false;
    }
    self.probe.start(target_bps, sending_bps, sending_bps, now);
    true
  }
}

impl Cluster {
  /// Carried when nothing was lost and the last train waited little longer than the first.
  fn outcome(&self, config: &RateProbeConfig) -> ProbeOutcome {
    let fell_short = ProbeOutcome::FellShort {
      target_bps: self.target_bps,
      floor_bps: self.floor_bps,
    };
    let is_complete = self.trains.iter().all(|train| {
      train
        .datagrams
        .iter()
        .all(|report| matches!(report, Report::Arrived(_)))
    });
    let (Some(first), Some(last)) = (self.trains.first(), self.trains.last()) else {
      return fell_short;
    };
    let (Some(first_delay_us), Some(last_delay_us)) = (
      first.least_delay_us(self.started_at),
      last.least_delay_us(self.started_at),
    ) else {
      return fell_short;
    };
    let growth_us = last_delay_us - first_delay_us;
    let limit_us = i64::try_from(config.max_queue_growth.as_micros()).unwrap_or(i64::MAX);
    if is_complete && self.trains.len() >= 2 && growth_us <= limit_us {
      ProbeOutcome::Carried {
        target_bps: self.target_bps,
      }
    } else {
      fell_short
    }
  }
}

impl Train {
  /// The least one-way delay of the train's datagrams in µs, on the receiver's clock less the
  /// time since `origin` on the sender's: the offset between the two clocks is the same for
  /// every train of a cluster.
  fn least_delay_us(&self, origin: Instant) -> Option<i64> {
    let sent_us =
      i64::try_from(self.sent_at.saturating_duration_since(origin).as_micros()).unwrap_or(i64::MAX);
    self
      .datagrams
      .iter()
      .filter_map(|report| match report {
        Report::Arrived(arrival_us) => Some(i64::try_from(*arrival_us).unwrap_or(i64::MAX) - sent_us),
        Report::Pending | Report::Lost => None,
      })
      .min()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_link_that_drains_every_train_carries_the_rate() {
    let config = RateProbeConfig::default();
    let mut probe = RateProbe::new();
    let start = Instant::now();
    // 2 Mbit/s target, 1 Mbit/s sent anyway: 1 Mbit/s of padding, 12.5 kB a train.
    let outcome = run_cluster(&mut probe, &config, start, 2_000_000, 1_000_000, |_, _| 0);
    assert_eq!(
      outcome,
      Some(ProbeOutcome::Carried {
        target_bps: 2_000_000
      })
    );
    assert_eq!(
      probe.shown_bps(&config, start + Duration::from_secs(1)),
      2_000_000
    );
    assert_eq!(probe.shown_bps(&config, start + Duration::from_secs(4)), 0);
    assert!(probe.may_start(start + Duration::from_secs(1)));
  }

  /// A token bucket lets the first train through at once, then every train waits longer: the
  /// spread of the first alone would have shown the line rate.
  #[test]
  fn a_queue_that_grows_from_train_to_train_falls_short() {
    let config = RateProbeConfig::default();
    let mut probe = RateProbe::new();
    let start = Instant::now();
    let outcome = run_cluster(&mut probe, &config, start, 6_000_000, 2_000_000, |train, _| {
      train * 60_000
    });
    assert_eq!(
      outcome,
      Some(ProbeOutcome::FellShort {
        target_bps: 6_000_000,
        floor_bps: 1_000_000
      })
    );
    assert_eq!(probe.shown_bps(&config, start + Duration::from_secs(1)), 0);
  }

  #[test]
  fn lost_padding_falls_short() {
    let config = RateProbeConfig::default();
    let mut probe = RateProbe::new();
    let start = Instant::now();
    let outcome = run_cluster(&mut probe, &config, start, 2_000_000, 0, |train, datagram| {
      if train == 1 && datagram == 0 { u64::MAX } else { 0 }
    });
    assert!(matches!(outcome, Some(ProbeOutcome::FellShort { .. })));
  }

  /// Clusters that fall short come 1 s, then 2 s apart, never further (#1792); one that gets
  /// through ends the wait.
  #[test]
  fn clusters_that_fall_short_back_off_for_seconds_only() {
    let config = RateProbeConfig::default();
    let mut probe = RateProbe::new();
    let mut now = Instant::now();
    let mut waits = Vec::new();
    for _ in 0..4 {
      run_cluster(&mut probe, &config, now, 6_000_000, 0, |train, _| train * 60_000);
      let reported = now + config.train_spacing * 2;
      let wait = (1..=20)
        .map(Duration::from_secs)
        .find(|wait| probe.may_start(reported + *wait))
        .unwrap();
      waits.push(wait.as_secs());
      now = reported + wait;
    }
    assert_eq!(waits, vec![1, 2, 2, 2]);
    run_cluster(&mut probe, &config, now, 2_000_000, 0, |_, _| 0);
    assert!(probe.may_start(now + config.train_spacing * 2));
  }

  /// Clusters climb from what the link carries in steps, smaller ones after clusters that fell
  /// short, larger ones after two in a row that got through.
  #[test]
  fn clusters_probe_a_step_above_what_the_link_carries() {
    let config = RateProbeConfig::default();
    let mut probe = RateProbe::new();
    let mut now = Instant::now();
    assert_eq!(probe.target_bps(&config, 7_000_000, 2_000_000), 3_000_000);
    assert_eq!(probe.target_bps(&config, 2_400_000, 2_000_000), 2_400_000);
    let mut targets = Vec::new();
    for _ in 0..3 {
      run_cluster(&mut probe, &config, now, 6_000_000, 0, |train, _| train * 60_000);
      targets.push(probe.target_bps(&config, 7_000_000, 2_000_000));
      now += Duration::from_secs(5);
    }
    assert_eq!(targets, vec![2_500_000, 2_500_000, 2_500_000]);
    run_cluster(&mut probe, &config, now, 2_000_000, 0, |_, _| 0);
    assert_eq!(probe.target_bps(&config, 7_000_000, 2_000_000), 3_000_000);
    // Two in a row: the link has room, and the steps double.
    run_cluster(
      &mut probe,
      &config,
      now + Duration::from_secs(1),
      3_000_000,
      0,
      |_, _| 0,
    );
    assert_eq!(probe.target_bps(&config, 7_000_000, 3_000_000), 6_000_000);
  }

  #[test]
  fn trains_follow_the_padding_rate_and_the_timer() {
    let config = RateProbeConfig::default();
    let mut probe = RateProbe::new();
    let now = Instant::now();
    assert_eq!(probe.due_train(&config, now), None);
    probe.start(10_000_000, 1_000_000, 2_000_000, now);
    // 8 Mbit/s for 100 ms: 100 kB.
    assert_eq!(probe.due_train(&config, now).map(|sizes| sizes.len()), Some(100));
    probe.sent(TransportSeq(0), &[1000; 100], now);
    assert_eq!(probe.due_train(&config, now + Duration::from_millis(70)), None);
    // A timer 150 ms later: 150 ms of padding, within the most a train takes.
    let late = probe
      .due_train(&config, now + Duration::from_millis(150))
      .unwrap();
    assert_eq!(late.len(), 100);
    // What is sent anyway covers the target: an eighth of it as padding all the same.
    let mut small = RateProbe::new();
    small.start(800_000, 100_000, 1_000_000, now);
    assert_eq!(small.due_train(&config, now).map(|sizes| sizes.len()), Some(2));
  }

  /// #1893: a voice's probes that fall short wait seconds, then up to 40 s, not 1 to 2 s.
  #[test]
  fn a_voice_probes_a_thin_link_rarely() {
    let config = RateProbeConfig::for_voice();
    let mut probe = RateProbe::new();
    let mut now = Instant::now();
    let mut waits = Vec::new();
    for _ in 0..5 {
      probe.start(80_000, 25_000, 25_000, now);
      probe.finish(
        &config,
        ProbeOutcome::FellShort {
          target_bps: 80_000,
          floor_bps: 25_000,
        },
        now,
      );
      let next = (1..=60)
        .map(|secs| now + Duration::from_secs(secs))
        .find(|at| probe.may_start(*at))
        .unwrap();
      waits.push(next.duration_since(now).as_secs());
      now = next;
    }
    assert_eq!(waits, vec![5, 10, 20, 40, 40]);
  }

  /// #1893: a thin link is probed at its rate, not at two whole datagrams a train. 32 kbit/s of
  /// padding for 100 ms are 400 bytes: two of 200; less than two of the smallest gets those.
  #[test]
  fn trains_of_a_thin_link_are_small() {
    let config = RateProbeConfig::default();
    let now = Instant::now();
    let mut thin = RateProbe::new();
    thin.start(50_000, 18_000, 18_000, now);
    assert_eq!(thin.due_train(&config, now), Some(vec![200, 200]));
    let mut thinnest = RateProbe::new();
    thinnest.start(20_000, 18_000, 18_000, now);
    assert_eq!(
      thinnest.due_train(&config, now),
      Some(vec![config.min_datagram_bytes; 2])
    );
  }

  #[test]
  fn a_cluster_without_feedback_is_given_up() {
    let config = RateProbeConfig::default();
    let mut probe = RateProbe::new();
    let now = Instant::now();
    probe.start(2_000_000, 0, 0, now);
    let sizes = probe.due_train(&config, now).unwrap();
    probe.sent(TransportSeq(0), &sizes, now);
    probe.on_timer(&config, now + Duration::from_millis(900));
    assert!(probe.is_running());
    probe.on_timer(&config, now + Duration::from_secs(1));
    assert!(!probe.is_running());
    assert!(!probe.may_start(now + Duration::from_millis(1900)));
    assert!(probe.may_start(now + Duration::from_secs(2)));
  }

  /// #1792: a laptop sharing its screen from LTE on the motorway (`ltevary.sh`): its uplink swings
  /// between 1.2 and 2.1 Mbit/s every 2 to 4 s, then a cell has 10 Mbit/s for a minute, then it
  /// swings again. The share's 1440p layer fits only the minute.
  mod uplink_on_lte {
    use std::collections::VecDeque;

    use teamcraft_proto::{VideoLayerInfo, VideoTrackKind};

    use super::*;
    use crate::bwe::{BweConfig, FeedbackRecorder};
    use crate::datagram::{DatagramHeader, StreamId};
    use crate::join_probe::{JoinProbe, JoinProbeConfig};
    use crate::uplink_layers::{Adaptation, OwnTrack, UplinkLayers, UplinkPolicy, probe_demand_bps};

    const TICK: Duration = Duration::from_millis(5);
    const ONE_WAY: Duration = Duration::from_millis(30);
    const FEEDBACK_INTERVAL: Duration = Duration::from_millis(100);
    const TIMER_INTERVAL: Duration = Duration::from_millis(100);
    const FRAME_INTERVAL_US: u64 = 33_333;
    const AUDIO_INTERVAL: Duration = Duration::from_millis(20);
    const AUDIO_BYTES: usize = 120;
    const PACKET_BYTES: usize = 1200;
    const SHARE: StreamId = StreamId(7);
    const SHARE_FROM: Duration = Duration::from_secs(2);
    const SWING_FROM: Duration = Duration::from_secs(5);
    const FAST_FROM: Duration = Duration::from_secs(40);
    const FAST_UNTIL: Duration = Duration::from_secs(100);
    const END: Duration = Duration::from_secs(140);

    #[test]
    fn a_sender_on_lte_follows_its_uplink_within_seconds() {
      let report = run(true);
      eprintln!("probing: {report:?}");
      eprintln!("without: {:?}", run(false));
      assert!(
        report
          .top_after
          .is_some_and(|after| after <= Duration::from_secs(5)),
        "top layer {:?} after the room appeared",
        report.top_after
      );
      assert!(
        report
          .whole_after
          .is_some_and(|after| after <= Duration::from_secs(8)),
        "top layer whole {:?} after the room appeared",
        report.whole_after
      );
      assert!(
        report.fits_after <= Duration::from_secs(1),
        "sending fit the link {:?} after the room went",
        report.fits_after
      );
      // Audio never stops: no gap a jitter buffer and Opus's own concealment can't bridge.
      assert!(
        report.longest_audio_gap <= Duration::from_millis(200),
        "audio stopped for {:?} ({:?} of {:?} lost)",
        report.longest_audio_gap,
        report.audio_lost,
        report.audio_sent
      );
      assert!(
        report.longest_video_gap <= Duration::from_millis(700),
        "video stood still for {:?} ({:?} of {:?} lost, padding {:?})",
        report.longest_video_gap,
        report.video_lost,
        report.video_sent,
        report.padding
      );
    }

    /// A user's uplink as it really is, not a fixed limit (#1792): seeded walks around 1.7 Mbit/s
    /// with dips, handovers and minutes of room ([`Walk`]). After every dip and every end of room
    /// what the sender sends fits the link again within a second; within 5 s of room appearing
    /// the top layer is sent; video and audio never stand still outside a handover.
    #[test]
    fn a_sender_follows_a_fluctuating_uplink_within_seconds() {
      for seed in [1, 7, 42] {
        let walk = Walk::new(
          seed,
          WALK_END,
          (1_200_000, 1_700_000, 2_400_000),
          (9_000_000, 18_000_000),
        );
        let (sent, ticks) = simulate(true, &|at| walk.capacity_at(at), &walk.outages, WALK_END);
        let report = WalkReport::of(&walk, &sent, &ticks);
        eprintln!("seed {seed}: {report:?}");
        assert!(
          report.rooms > 0 && report.drops > 5,
          "seed {seed}: a walk without room or dips"
        );
        assert!(
          report.slowest_top <= Duration::from_secs(5),
          "seed {seed}: top layer {:?} after room appeared",
          report.slowest_top
        );
        // About a second: the link's bucket hides a dip for a third of a second, its queue
        // fills in a few hundred milliseconds, and loss shows a reorder window (150 ms) and a
        // feedback interval later.
        assert!(
          report.slowest_fit <= Duration::from_millis(1200),
          "seed {seed}: sending fit {:?} after a drop",
          report.slowest_fit
        );
        assert!(
          report.longest_video_gap <= Duration::from_millis(700),
          "seed {seed}: video stood still for {:?}",
          report.longest_video_gap
        );
        assert!(
          report.longest_audio_gap <= Duration::from_millis(200),
          "seed {seed}: audio stopped for {:?}",
          report.longest_audio_gap
        );
      }
    }

    const WALK_END: Duration = Duration::from_secs(200);
    /// Sending fits a fluctuating link when it is within this factor of it ([`WalkReport`]).
    const FIT_TOLERANCE: f64 = 1.15;

    /// How a sender followed a [`Walk`]: the slowest of its times.
    #[derive(Debug)]
    struct WalkReport {
      rooms: usize,
      drops: usize,
      /// From room appearing until the top layer is sent.
      slowest_top: Duration,
      /// From a dip or the end of room until what is sent fits the link.
      slowest_fit: Duration,
      /// The longest time without an arrival, outside handovers and the second after them.
      longest_video_gap: Duration,
      longest_audio_gap: Duration,
    }

    impl WalkReport {
      fn of(walk: &Walk, sent: &[Sent], ticks: &[Tick]) -> Self {
        let after = |from: Duration, is_done: &dyn Fn(&Tick) -> bool| {
          ticks
            .iter()
            .filter(|tick| tick.at >= from && tick.at >= SWING_FROM)
            .find(|tick| is_done(tick))
            .map_or(WALK_END.saturating_sub(from), |tick| tick.at - from)
        };
        let slowest_top = walk
          .rooms
          .iter()
          .map(|(from, _)| after(*from, &|tick| tick.is_top_sent))
          .max()
          .unwrap_or_default();
        // Within a step of the estimate: an excess smaller than that fills the link's bucket and
        // queue for seconds, so no estimate sees it within a dip, and nothing stands still.
        let slowest_fit = walk
          .drops
          .iter()
          .map(|from| {
            after(*from, &|tick| {
              f64::from(tick.planned_bps) <= FIT_TOLERANCE * tick.capacity_bps as f64
            })
          })
          .max()
          .unwrap_or_default();
        let longest_gap = |kind: Kind| {
          let mut arrivals: Vec<Duration> = sent
            .iter()
            .filter(|datagram| datagram.kind == kind && datagram.at >= SWING_FROM)
            .filter_map(|datagram| datagram.maybe_arrival)
            .collect();
          arrivals.sort_unstable();
          arrivals
            .windows(2)
            .filter(|pair| !walk.is_near_outage(pair[0], pair[1]))
            .map(|pair| pair[1] - pair[0])
            .max()
            .unwrap_or_default()
        };
        Self {
          rooms: walk.rooms.len(),
          drops: walk.drops.len(),
          slowest_top,
          slowest_fit,
          longest_video_gap: longest_gap(Kind::Video),
          longest_audio_gap: longest_gap(Kind::Audio),
        }
      }
    }

    /// How the sender followed its uplink: phases are the swing before the minute, the minute,
    /// and the swing after it.
    #[derive(Debug)]
    struct Report {
      /// From the room appearing until the top layer is sent, and until it is sent whole (an
      /// adaptive share sends it at a lower frame rate first).
      top_after: Option<Duration>,
      whole_after: Option<Duration>,
      /// From the room going until what the sender sends fits the link again.
      fits_after: Duration,
      longest_audio_gap: Duration,
      longest_video_gap: Duration,
      audio_lost: [usize; 3],
      audio_sent: [usize; 3],
      video_lost: [usize; 3],
      video_sent: [usize; 3],
      /// Padding sent, and dropped.
      padding: (usize, usize),
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Kind {
      Audio,
      Video,
      Padding,
    }

    /// What the sender planned at one tick of the simulation, on a link of `capacity_bps`.
    struct Tick {
      at: Duration,
      capacity_bps: u64,
      /// Audio and the video layers at their bitrates.
      planned_bps: u32,
      is_top_sent: bool,
      is_top_whole: bool,
    }

    /// One datagram on the link: when sent, of which kind, and when it arrived (`None`: dropped).
    struct Sent {
      at: Duration,
      kind: Kind,
      maybe_arrival: Option<Duration>,
    }

    /// Runs the share on a link of `capacity_at` until `end`; `is_probing`: with [`UplinkProbe`].
    /// What was sent, and per tick what the sender planned.
    fn simulate(
      is_probing: bool,
      capacity_at: &dyn Fn(Duration) -> u64,
      holds: &[(Duration, Duration)],
      end: Duration,
    ) -> (Vec<Sent>, Vec<Tick>) {
      let start = Instant::now();
      let config = RateProbeConfig::default();
      let policy = UplinkPolicy::default();
      let layers = share_layers();
      let tracks = [OwnTrack {
        stream: SHARE,
        kind: &VideoTrackKind::Screen,
        layers: &layers,
        adaptation: Adaptation::InPlace,
      }];
      let bwe = BweConfig::default();
      let mut uplink = SendSideEstimator::new(bwe);
      uplink.set_demand_bps(Some(probe_demand_bps(&tracks, false, &policy)));
      let mut allocation = UplinkLayers::new(bwe.start_bps);
      let mut probe = UplinkProbe::new();
      // As the call engine does, the uplink is probed at the start.
      let mut join_probe = JoinProbe::new(JoinProbeConfig::default(), start);
      let mut recorder = FeedbackRecorder::new(start);
      let mut link = Link::new(start, holds);
      let mut in_flight: VecDeque<(Instant, TransportSeq)> = VecDeque::new();
      let mut feedback_in_flight: VecDeque<(Instant, TransportFeedback)> = VecDeque::new();
      let mut sent: Vec<Sent> = Vec::new();
      let mut next_frame_us = 0u64;
      let mut ticks: Vec<Tick> = Vec::new();
      let mut send = |link: &mut Link,
                      kind: Kind,
                      bytes: usize,
                      seq: TransportSeq,
                      now: Instant,
                      in_flight: &mut VecDeque<_>| {
        let maybe_arrival = link.send(bytes, now);
        if let Some(arrival) = maybe_arrival {
          in_flight.push_back((arrival, seq));
        }
        sent.push(Sent {
          at: now - start,
          kind,
          maybe_arrival: maybe_arrival.map(|arrival| arrival - start),
        });
      };
      let mut now = start;
      while now - start < end {
        let elapsed = now - start;
        link.capacity_bps = capacity_at(elapsed);
        if elapsed.as_micros().is_multiple_of(AUDIO_INTERVAL.as_micros()) {
          let seq = uplink.on_send(AUDIO_BYTES, now);
          send(&mut link, Kind::Audio, AUDIO_BYTES, seq, now, &mut in_flight);
        }
        if elapsed == SHARE_FROM {
          // The share starts once the call runs, as the call engine starts an own track.
          uplink.measure_afresh();
          allocation.measure_afresh(uplink.estimate_bps());
          next_frame_us = u64::try_from(SHARE_FROM.as_micros()).unwrap();
        }
        if elapsed >= SHARE_FROM && u64::try_from(elapsed.as_micros()).unwrap() >= next_frame_us {
          next_frame_us += FRAME_INTERVAL_US;
          for info in &layers {
            if !allocation.allows(SHARE, info.layer) {
              continue;
            }
            let frame_bytes = (allocation.bitrate_bps(SHARE, info) / 8 / 30) as usize;
            for _ in 0..frame_bytes.div_ceil(PACKET_BYTES) {
              let seq = uplink.on_send(PACKET_BYTES, now);
              send(&mut link, Kind::Video, PACKET_BYTES, seq, now, &mut in_flight);
            }
          }
        }
        if elapsed.as_micros().is_multiple_of(TIMER_INTERVAL.as_micros()) {
          if let Some(measurement) = join_probe.on_timer(now) {
            uplink.apply_probe(measurement.capacity_bps);
          }
          if let Some(sizes) = join_probe.due_train(now) {
            let seqs: Vec<TransportSeq> = sizes
              .iter()
              .map(|bytes| uplink.on_send_probe(*bytes, now))
              .collect();
            for (bytes, seq) in sizes.iter().zip(&seqs) {
              send(&mut link, Kind::Padding, *bytes, *seq, now, &mut in_flight);
            }
            join_probe.sent(seqs[0], &sizes, now);
          }
          let maybe_wanted = allocation
            .is_holding_back(&tracks)
            .then(|| probe_demand_bps(&tracks, false, &policy));
          if is_probing && join_probe.is_over() {
            for datagram in probe.on_timer(&config, &mut uplink, maybe_wanted, now) {
              let seq = DatagramHeader::parse(&datagram)
                .unwrap()
                .maybe_transport_seq
                .unwrap();
              send(&mut link, Kind::Padding, datagram.len(), seq, now, &mut in_flight);
            }
          }
        }
        while in_flight.front().is_some_and(|(arrival, _)| *arrival <= now) {
          let (arrival, seq) = in_flight.pop_front().unwrap();
          recorder.on_arrival(seq, arrival);
        }
        if elapsed.as_micros().is_multiple_of(FEEDBACK_INTERVAL.as_micros())
          && let Some(feedback) = recorder.take_feedback(now)
        {
          feedback_in_flight.push_back((now + ONE_WAY, feedback));
        }
        while feedback_in_flight.front().is_some_and(|(at, _)| *at <= now) {
          let (_, feedback) = feedback_in_flight.pop_front().unwrap();
          if let Some(measurement) = join_probe.on_feedback(&feedback) {
            uplink.apply_probe(measurement.capacity_bps);
          }
          uplink.on_feedback(&feedback, now, start);
          if is_probing {
            probe.on_feedback(&config, &feedback, &mut uplink, now);
          }
          allocation.on_estimate(uplink.estimate_bps(), &tracks, false, &policy, now);
        }
        let planned_bps: u32 = policy.audio_reserve_bps
          + layers
            .iter()
            .filter(|info| allocation.allows(SHARE, info.layer))
            .map(|info| allocation.bitrate_bps(SHARE, info))
            .sum::<u32>();
        ticks.push(Tick {
          at: elapsed,
          capacity_bps: link.capacity_bps,
          planned_bps,
          is_top_sent: allocation.allows(SHARE, 2),
          is_top_whole: allocation.allows(SHARE, 2)
            && allocation.bitrate_bps(SHARE, &layers[2]) == layers[2].max_bitrate_bps,
        });
        now += TICK;
      }
      (sent, ticks)
    }

    /// Runs the share on the fixed LTE link ([`capacity_at`]) for [`END`].
    fn run(is_probing: bool) -> Report {
      let (sent, ticks) = simulate(is_probing, &capacity_at, &[], END);
      let in_fast = |tick: &&Tick| tick.at >= FAST_FROM && tick.at < FAST_UNTIL;
      let top_after = ticks
        .iter()
        .filter(in_fast)
        .find(|tick| tick.is_top_sent)
        .map(|tick| tick.at - FAST_FROM);
      let whole_after = ticks
        .iter()
        .filter(in_fast)
        .find(|tick| tick.is_top_whole)
        .map(|tick| tick.at - FAST_FROM);
      let fits_after = ticks
        .iter()
        .filter(|tick| tick.at >= FAST_UNTIL)
        .find(|tick| u64::from(tick.planned_bps) <= tick.capacity_bps)
        .map_or(END - FAST_UNTIL, |tick| tick.at - FAST_UNTIL);
      let phases = [
        (SWING_FROM, FAST_FROM),
        (FAST_FROM, FAST_UNTIL),
        (FAST_UNTIL, END),
      ];
      let count = |kind: Kind, is_lost: bool| {
        phases.map(|(from, until)| {
          sent
            .iter()
            .filter(|datagram| datagram.kind == kind && datagram.at >= from && datagram.at < until)
            .filter(|datagram| !is_lost || datagram.maybe_arrival.is_none())
            .count()
        })
      };
      let longest_gap = |kind: Kind| {
        let mut arrivals: Vec<Duration> = sent
          .iter()
          .filter(|datagram| datagram.kind == kind && datagram.at >= SWING_FROM)
          .filter_map(|datagram| datagram.maybe_arrival)
          .collect();
        arrivals.sort_unstable();
        arrivals
          .windows(2)
          .map(|pair| pair[1] - pair[0])
          .max()
          .unwrap_or_default()
      };
      let padding_sent = sent.iter().filter(|datagram| datagram.kind == Kind::Padding);
      Report {
        top_after,
        whole_after,
        fits_after,
        longest_audio_gap: longest_gap(Kind::Audio),
        longest_video_gap: longest_gap(Kind::Video),
        audio_lost: count(Kind::Audio, true),
        audio_sent: count(Kind::Audio, false),
        video_lost: count(Kind::Video, true),
        video_sent: count(Kind::Video, false),
        padding: (
          padding_sent.clone().count(),
          padding_sent
            .filter(|datagram| datagram.maybe_arrival.is_none())
            .count(),
        ),
      }
    }

    /// The uplink of `ltevary.sh`: 60 % of the downlink's swings, 10 Mbit/s for the minute.
    fn capacity_at(elapsed: Duration) -> u64 {
      const SWING_KBPS: [u64; 8] = [1_200, 2_100, 1_440, 1_860, 1_200, 1_680, 2_040, 1_320];
      const HOLD_SECONDS: [u64; 5] = [2, 3, 4, 3, 2];
      if (FAST_FROM..FAST_UNTIL).contains(&elapsed) {
        return 10_000_000;
      }
      let phase_start = if elapsed < FAST_FROM {
        Duration::ZERO
      } else {
        FAST_UNTIL
      };
      let mut at = phase_start;
      let mut index = 0;
      loop {
        let hold = Duration::from_secs(HOLD_SECONDS[index % HOLD_SECONDS.len()]);
        if elapsed < at + hold {
          return SWING_KBPS[index % SWING_KBPS.len()] * 1000;
        }
        at += hold;
        index += 1;
      }
    }

    /// A mobile link as a user has it (#1792), seeded: every 500 ms the rate walks by up to
    /// ±15 % within `(low, typical, high)`; now and then (4 % of the steps) a dip to 40 % for 1 to
    /// 2 s, a tunnel or a crowded cell, or (2 %) a handover that holds everything for 200 to
    /// 500 ms ([`Link`] hands it over then); and every 60 to 90 s a cell with room, 30 to 60 s within `rooms`.
    struct Walk {
      /// From when, which capacity.
      steps: Vec<(Duration, u64)>,
      /// When a dip began or room ended: the sender has to fit the link again.
      drops: Vec<Duration>,
      outages: Vec<(Duration, Duration)>,
      rooms: Vec<(Duration, Duration)>,
    }

    impl Walk {
      const STEP: Duration = Duration::from_millis(500);

      fn new(seed: u64, end: Duration, (low, typical, high): (u64, u64, u64), rooms: (u64, u64)) -> Self {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move || {
          state ^= state << 13;
          state ^= state >> 7;
          state ^= state << 17;
          state
        };
        let mut walk = Self {
          steps: Vec::new(),
          drops: Vec::new(),
          outages: Vec::new(),
          rooms: Vec::new(),
        };
        let mut rate = typical as f64;
        let mut at = Duration::ZERO;
        let mut next_room = Duration::from_secs(30 + next() % 20);
        while at < end {
          if at >= next_room && at + Duration::from_secs(45) < end {
            let length = Duration::from_secs(30 + next() % 31).min(end - at - Duration::from_secs(15));
            let mut room_rate = (rooms.0 + next() % (rooms.1 - rooms.0)) as f64;
            let until = at + length;
            walk.rooms.push((at, until));
            while at < until {
              walk.steps.push((at, room_rate as u64));
              room_rate =
                (room_rate * (0.9 + (next() % 200) as f64 / 1000.0)).clamp(rooms.0 as f64, rooms.1 as f64);
              at += Self::STEP;
            }
            walk.drops.push(at);
            next_room = at + Duration::from_secs(60 + next() % 30);
            continue;
          }
          let roll = next() % 100;
          if roll < 2 {
            let length = Duration::from_millis(200 + next() % 300);
            walk.outages.push((at, at + length));
            at += length;
          } else if roll < 6 {
            walk.steps.push((at, (rate * 0.4) as u64));
            walk.drops.push(at);
            at += Duration::from_millis(1000 + next() % 1000);
          } else {
            rate = (rate * (0.85 + (next() % 300) as f64 / 1000.0)).clamp(low as f64, high as f64);
            walk.steps.push((at, rate as u64));
            at += Self::STEP;
          }
        }
        walk
      }

      fn capacity_at(&self, at: Duration) -> u64 {
        self
          .steps
          .iter()
          .rev()
          .find(|(from, _)| *from <= at)
          .map_or(1_000_000, |(_, bps)| *bps)
      }

      /// Whether a gap from `from` to `until` touches a handover or the second after it.
      fn is_near_outage(&self, from: Duration, until: Duration) -> bool {
        self
          .outages
          .iter()
          .any(|(start, end)| from < *end + Duration::from_secs(1) && until > *start)
      }
    }

    /// A 1440p screen share's layers, about (#1733): 480p, 720p and 1440p.
    fn share_layers() -> Vec<VideoLayerInfo> {
      [
        (0, 852, 480, 500_000),
        (1, 1280, 720, 1_660_000),
        (2, 2560, 1440, 6_600_000),
      ]
      .into_iter()
      .map(|(layer, width, height, max_bitrate_bps)| VideoLayerInfo {
        layer,
        width,
        height,
        max_framerate: 30,
        max_bitrate_bps,
        maybe_codec: None,
      })
      .collect()
    }

    /// The test setup's link (`tbf` with a 32 kB bucket and 150 ms of queue), 30 ms one way.
    /// During a handover (`holds`) the link holds what it gets and hands it over once it ends.
    struct Link {
      capacity_bps: u64,
      /// When the bucket would be empty had nothing been sent since.
      empty_at: Instant,
      holds: Vec<(Instant, Instant)>,
      last_arrival: Instant,
    }

    impl Link {
      const BUCKET_BYTES: usize = 32_000;
      const QUEUE_LIMIT: Duration = Duration::from_millis(150);

      fn new(start: Instant, holds: &[(Duration, Duration)]) -> Self {
        Self {
          capacity_bps: 1_000_000,
          empty_at: start,
          holds: holds
            .iter()
            .map(|(from, until)| (start + *from, start + *until))
            .collect(),
          last_arrival: start,
        }
      }

      fn send(&mut self, bytes: usize, now: Instant) -> Option<Instant> {
        let seconds = |bytes: usize| Duration::from_secs_f64(bytes as f64 * 8.0 / self.capacity_bps as f64);
        let full_since = now.checked_sub(seconds(Self::BUCKET_BYTES)).unwrap_or(now);
        let empty_at = self.empty_at.max(full_since);
        if empty_at.saturating_duration_since(now) > Self::QUEUE_LIMIT {
          return None;
        }
        self.empty_at = empty_at + seconds(bytes);
        let through = self.empty_at.max(now);
        let through = self
          .holds
          .iter()
          .find(|(from, until)| through >= *from && through < *until)
          .map_or(through, |(_, until)| *until);
        self.last_arrival = (through + ONE_WAY).max(self.last_arrival);
        Some(self.last_arrival)
      }
    }
  }

  /// Runs a whole cluster from `start`, trains a spacing apart, numbered so they wrap; each
  /// datagram arrives `extra_delay_us(train, datagram)` after its send (`u64::MAX`: lost),
  /// reported in one feedback after the last train.
  fn run_cluster(
    probe: &mut RateProbe,
    config: &RateProbeConfig,
    start: Instant,
    target_bps: u32,
    sending_bps: u32,
    extra_delay_us: impl Fn(u64, u64) -> u64,
  ) -> Option<ProbeOutcome> {
    probe.start(target_bps, target_bps / 6, sending_bps, start);
    let first_seq = TransportSeq(65_530);
    let mut seq = first_seq;
    let mut arrivals_us = Vec::new();
    let mut now = start;
    for train in 0..u64::from(config.trains) {
      let sizes = probe.due_train(config, now).unwrap();
      probe.sent(seq, &sizes, now);
      for datagram in 0..sizes.len() as u64 {
        let sent_us = u64::try_from(now.duration_since(start).as_micros()).unwrap();
        let extra_us = extra_delay_us(train, datagram);
        // 20 ms one way, 100 µs apart on the link.
        arrivals_us.push((extra_us != u64::MAX).then(|| sent_us + 20_000 + datagram * 100 + extra_us));
        seq = seq.next();
      }
      if train + 1 < u64::from(config.trains) {
        now += config.train_spacing;
      }
    }
    let feedback = TransportFeedback {
      base_seq: first_seq.0,
      arrivals_us,
    };
    probe.on_feedback(config, &feedback, now)
  }
}
