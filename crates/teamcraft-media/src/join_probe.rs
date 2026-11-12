//! Measuring a link when a call starts (#1762): a rough but reliable idea of what it carries, so
//! the first layers and the bandwidth estimate fit the link instead of starting low, growing fast
//! and collapsing on a narrow one (Jitsi and MatrixRTC probe the same way).
//!
//! A few short trains of padding datagrams go out back to back, a moment apart, numbered for the
//! receiver's transport feedback like any datagram. The narrowest link on the way spaces each
//! train out at its rate; the feedback's arrival times show the spread, and the bytes after the
//! first arrival over the time they took is that rate (packet-train dispersion). The median of the
//! trains is the result, so one disturbed by other traffic or the receiver's scheduling doesn't
//! decide it. The client probes its uplink towards the SFU, the SFU each receiver's downlink.
//!
//! WebRTC (and so Jitsi's and MatrixRTC's clients) probes at the start with paced clusters at 3×
//! and 6× the start rate and takes the lower of their send and receive rate; Jitsi's bridge then
//! pads towards receivers up to its estimate. Trains sent back to back need no pacer on either
//! side and show the bottleneck in one round trip; like WebRTC's clusters, a train counts only
//! when most of it arrived.
//!
//! - **Bounded:** a few trains of a few datagrams ([`JoinProbeConfig`], 15 kB by default), within
//!   a second of the join; audio isn't held back (padding waits behind it in the send queues), and
//!   nothing waits for the result: the estimate starts as before and takes it when it comes.
//! - **Rough:** QUIC paces a young connection, and a fast link spreads a train less than the
//!   receiver's timing resolves, so on fast links the result is a lower bound; the estimate keeps
//!   growing from it. Loss inside a train widens its spread and lowers the result.
//! - **Nothing to learn:** padding carries zeros; the receiver sees only how much arrived when,
//!   which it sees of every datagram anyway.

use std::time::Duration;

use teamcraft_proto::TransportFeedback;
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::datagram::TransportSeq;

/// A train counts only when at least this share of its datagrams arrived (WebRTC's probe
/// estimator takes the same, `kMinReceivedProbesRatio`): a train that lost more says more about
/// the loss than about the rate.
const MIN_RECEIVED_SHARE: f64 = 0.8;

/// How a link is probed at the start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JoinProbeConfig {
  /// Trains sent; the median of those measured is the result.
  pub trains: u8,
  /// Datagrams per train, sent back to back: enough to show a spread, few enough that QUIC sends
  /// them as one burst (its initial window holds about ten).
  pub datagrams_per_train: u8,
  /// Bytes per datagram: large enough that each goes in a packet of its own.
  pub datagram_bytes: usize,
  /// The time between two trains, so the queue of one has drained before the next.
  pub spacing: Duration,
  /// A train whose feedback hasn't come within this is given up.
  pub timeout: Duration,
}

/// What the probe showed of a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkMeasurement {
  /// What the link carries, in bit/s: the median of the trains' spreads.
  pub capacity_bps: u32,
  /// Padding datagrams lost, in percent of those sent.
  pub loss_percent: u8,
}

/// The probe of one link, from the first train to its result.
#[derive(Debug)]
pub struct JoinProbe {
  config: JoinProbeConfig,
  trains: Vec<Train>,
  maybe_next_at: Option<Instant>,
  maybe_result: Option<LinkMeasurement>,
  is_over: bool,
}

#[derive(Debug)]
struct Train {
  first_seq: TransportSeq,
  sent_at: Instant,
  /// Per datagram: its bytes and what the feedback said of it.
  datagrams: Vec<(usize, Report)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
  Pending,
  Lost,
  /// Arrival in µs on the receiver's clock.
  Arrived(u64),
}

impl Default for JoinProbeConfig {
  fn default() -> Self {
    Self {
      trains: 3,
      datagrams_per_train: 5,
      datagram_bytes: 1000,
      spacing: Duration::from_millis(150),
      timeout: Duration::from_secs(1),
    }
  }
}

impl JoinProbeConfig {
  /// The bytes the whole probe sends.
  pub fn total_bytes(&self) -> usize {
    usize::from(self.trains) * usize::from(self.datagrams_per_train) * self.datagram_bytes
  }

  /// These settings with as many trains as fit in `max_bytes`; `None` when not even one does
  /// (probing off).
  pub fn within_bytes(self, max_bytes: usize) -> Option<Self> {
    let train_bytes = usize::from(self.datagrams_per_train) * self.datagram_bytes;
    if train_bytes == 0 {
      return None;
    }
    let trains = u8::try_from(max_bytes / train_bytes)
      .unwrap_or(u8::MAX)
      .min(self.trains);
    (trains > 0).then_some(Self { trains, ..self })
  }
}

impl JoinProbe {
  /// A probe whose first train is due at `now`.
  pub fn new(config: JoinProbeConfig, now: Instant) -> Self {
    Self {
      config,
      trains: Vec::new(),
      maybe_next_at: Some(now),
      maybe_result: None,
      is_over: config.trains == 0 || config.datagrams_per_train < 2,
    }
  }

  /// The datagram sizes of the next train, when one is due at `now`.
  pub fn due_train(&self, now: Instant) -> Option<Vec<usize>> {
    let is_due = !self.is_over
      && self.trains.len() < usize::from(self.config.trains)
      && self.maybe_next_at.is_some_and(|next| now >= next);
    is_due.then(|| vec![self.config.datagram_bytes; usize::from(self.config.datagrams_per_train)])
  }

  /// A train of `sizes` went out at `now`, numbered from `first_seq` on.
  pub fn sent(&mut self, first_seq: TransportSeq, sizes: &[usize], now: Instant) {
    self.trains.push(Train {
      first_seq,
      sent_at: now,
      datagrams: sizes.iter().map(|bytes| (*bytes, Report::Pending)).collect(),
    });
    self.maybe_next_at =
      (self.trains.len() < usize::from(self.config.trains)).then_some(now + self.config.spacing);
  }

  /// Notes the feedback's reports of the trains' datagrams; the result, once, when the last train
  /// is reported.
  pub fn on_feedback(&mut self, feedback: &TransportFeedback) -> Option<LinkMeasurement> {
    for train in &mut self.trains {
      for (offset, maybe_arrival_us) in feedback.arrivals_us.iter().enumerate() {
        // Feedback numbers are 16 bits wide like the sequence numbers; a train is a few of them.
        let seq = feedback.base_seq.wrapping_add(offset as u16);
        let index = usize::from(seq.wrapping_sub(train.first_seq.0));
        if let Some((_, report)) = train.datagrams.get_mut(index) {
          *report = maybe_arrival_us.map_or(Report::Lost, Report::Arrived);
        }
      }
    }
    self.finish_if_reported()
  }

  /// Gives up trains whose feedback doesn't come; the result, once, if that was the last one
  /// missing.
  pub fn on_timer(&mut self, now: Instant) -> Option<LinkMeasurement> {
    for train in &mut self.trains {
      if now.saturating_duration_since(train.sent_at) >= self.config.timeout {
        for (_, report) in &mut train.datagrams {
          if *report == Report::Pending {
            *report = Report::Lost;
          }
        }
      }
    }
    self.finish_if_reported()
  }

  /// What the probe showed, once it is over; `None` before, and when no train could be measured.
  pub fn result(&self) -> Option<LinkMeasurement> {
    self.maybe_result
  }

  /// Every train went out and was reported (or given up).
  pub fn is_over(&self) -> bool {
    self.is_over
  }

  fn finish_if_reported(&mut self) -> Option<LinkMeasurement> {
    let is_reported = self.trains.len() == usize::from(self.config.trains)
      && self.trains.iter().all(|train| {
        train
          .datagrams
          .iter()
          .all(|(_, report)| *report != Report::Pending)
      });
    if self.is_over || !is_reported {
      return None;
    }
    self.is_over = true;
    let mut rates: Vec<u32> = self.trains.iter().filter_map(Train::rate_bps).collect();
    rates.sort_unstable();
    // The lower median: of two, the smaller one.
    let capacity_bps = *rates.get(rates.len().saturating_sub(1) / 2)?;
    let sent: usize = self.trains.iter().map(|train| train.datagrams.len()).sum();
    let lost = self
      .trains
      .iter()
      .flat_map(|train| &train.datagrams)
      .filter(|(_, report)| *report == Report::Lost)
      .count();
    let loss_percent = u8::try_from(lost * 100 / sent.max(1)).unwrap_or(100);
    self.maybe_result = Some(LinkMeasurement {
      capacity_bps,
      loss_percent,
    });
    self.maybe_result
  }
}

impl Train {
  fn rate_bps(&self) -> Option<u32> {
    let arrivals: Vec<(u64, usize)> = self
      .datagrams
      .iter()
      .filter_map(|(bytes, report)| match report {
        Report::Arrived(arrival_us) => Some((*arrival_us, *bytes)),
        Report::Pending | Report::Lost => None,
      })
      .collect();
    if (arrivals.len() as f64) < MIN_RECEIVED_SHARE * self.datagrams.len() as f64 {
      return None;
    }
    train_rate_bps(&arrivals)
  }
}

/// The rate the arrivals of datagrams sent back to back show (arrival in µs, bytes): the bytes
/// after the first arrival over the time they took. `None` with fewer than two; a spread too
/// small to time is `u32::MAX` (the caller caps it).
fn train_rate_bps(arrivals: &[(u64, usize)]) -> Option<u32> {
  let mut sorted = arrivals.to_vec();
  sorted.sort_unstable();
  let (&(first_us, _), &(last_us, _)) = (sorted.first()?, sorted.last()?);
  if sorted.len() < 2 {
    return None;
  }
  let bits = sorted.iter().skip(1).map(|(_, bytes)| *bytes).sum::<usize>() as f64 * 8.0;
  let span_us = last_us.saturating_sub(first_us);
  if span_us == 0 {
    return Some(u32::MAX);
  }
  // Saturating float-to-int conversion is intended.
  Some((bits * 1_000_000.0 / span_us as f64) as u32)
}

#[cfg(test)]
mod tests {
  use super::*;

  mod probe {
    use super::*;

    #[test]
    fn trains_go_out_a_moment_apart_and_the_median_is_the_result() {
      let start = Instant::now();
      let mut probe = JoinProbe::new(JoinProbeConfig::default(), start);
      let mut seq = 0u16;
      let mut at = start;
      // The trains arrive spread as 2, 3 and 20 Mbit/s links spread them: the median is 3.
      for rate_bps in [2_000_000.0, 3_000_000.0, 20_000_000.0] {
        let sizes = probe.due_train(at).expect("a train is due");
        assert_eq!(sizes, vec![1000; 5]);
        probe.sent(TransportSeq(seq), &sizes, at);
        assert_eq!(probe.due_train(at + Duration::from_millis(100)), None, "not yet");
        let gap_us = 8000.0 / rate_bps * 1_000_000.0;
        let feedback = TransportFeedback {
          base_seq: seq,
          arrivals_us: (0..5)
            .map(|index| Some((f64::from(index) * gap_us) as u64))
            .collect(),
        };
        let maybe_result = probe.on_feedback(&feedback);
        assert_eq!(maybe_result.is_some(), seq == 10, "only after the last train");
        seq += 5;
        at += JoinProbeConfig::default().spacing;
      }
      let result = probe.result().unwrap();
      assert!(
        (2_900_000..=3_100_000).contains(&result.capacity_bps),
        "{result:?}"
      );
      assert_eq!(result.loss_percent, 0);
      assert!(probe.is_over());
      assert_eq!(probe.due_train(at), None, "no more trains");
    }

    #[test]
    fn trains_without_feedback_are_given_up() {
      let now = Instant::now();
      let mut probe = JoinProbe::new(one_train(), now);
      let sizes = probe.due_train(now).unwrap();
      probe.sent(TransportSeq(65_533), &sizes, now);
      assert_eq!(probe.on_timer(now + Duration::from_millis(500)), None);
      assert_eq!(
        probe.on_timer(now + Duration::from_secs(1)),
        None,
        "nothing measured"
      );
      assert!(probe.is_over());
    }

    #[test]
    fn lost_datagrams_count_and_numbers_wrap() {
      let now = Instant::now();
      let mut probe = JoinProbe::new(one_train(), now);
      let sizes = probe.due_train(now).unwrap();
      probe.sent(TransportSeq(65_534), &sizes, now);
      let feedback = TransportFeedback {
        base_seq: 65_534,
        arrivals_us: vec![Some(0), None, Some(8000), Some(12_000), Some(16_000)],
      };
      let result = probe.on_feedback(&feedback).unwrap();
      assert_eq!(result.loss_percent, 20);
      // Three datagrams after the first in 16 ms: 1.5 Mbit/s.
      assert_eq!(result.capacity_bps, 1_500_000);
    }

    #[test]
    fn a_train_that_lost_much_of_itself_is_not_measured() {
      let now = Instant::now();
      let mut probe = JoinProbe::new(one_train(), now);
      let sizes = probe.due_train(now).unwrap();
      probe.sent(TransportSeq(0), &sizes, now);
      let feedback = TransportFeedback {
        base_seq: 0,
        arrivals_us: vec![Some(0), None, None, Some(12_000), Some(16_000)],
      };
      assert_eq!(probe.on_feedback(&feedback), None);
      assert!(probe.is_over());
    }

    #[test]
    fn the_budget_bounds_the_trains() {
      let config = JoinProbeConfig::default();
      assert_eq!(config.total_bytes(), 15_000);
      assert_eq!(config.within_bytes(10_000).unwrap().trains, 2);
      assert_eq!(config.within_bytes(100_000).unwrap().trains, 3);
      assert_eq!(config.within_bytes(4_999), None);
    }

    fn one_train() -> JoinProbeConfig {
      JoinProbeConfig {
        trains: 1,
        ..JoinProbeConfig::default()
      }
    }
  }

  #[test]
  fn a_train_rate_needs_two_arrivals() {
    assert_eq!(train_rate_bps(&[(5, 1000)]), None);
    assert_eq!(train_rate_bps(&[(5, 1000), (5, 1000)]), Some(u32::MAX));
    assert_eq!(train_rate_bps(&[(8000, 1000), (0, 1000)]), Some(1_000_000));
  }
}
