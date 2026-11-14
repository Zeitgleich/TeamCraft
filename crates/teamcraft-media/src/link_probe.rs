//! Measuring the link before the call (#1959): the client's side of a link probe session.
//!
//! On the pre-join screen or in a meeting's lobby the client opens a connection to the SFU with a
//! link probe ticket ([`teamcraft_proto::CallClientMessage::Probe`]). It probes its uplink with the
//! trains of the join's probe ([`crate::join_probe`]), which the SFU answers with transport
//! feedback, and reports the SFU's padding back, so the SFU measures the downlink and says what it
//! showed. The result ([`PriorLink`]) goes to the call engine at the join
//! ([`crate::call_engine::CallEngine::set_prior_link`]), which then starts its voice step, uplink
//! estimate and downlink measurement from it instead of probing at the join.
//!
//! Nothing waits for it: a join before it ended, an SFU that takes no probe tickets, or a failure
//! leaves the call measuring at the join, as before.
//!
//! Pure: time is passed in.

use std::time::Duration;

use teamcraft_proto::{CallEvent, TransportFeedback};
// `std::time::Instant` on native; the browser clock on wasm, where `std` has none.
use web_time::Instant;

use crate::bwe::FeedbackRecorder;
use crate::datagram::{DatagramHeader, PADDING_STREAM, TransportSeq, padding_datagram};
use crate::join_probe::{JoinProbe, JoinProbeConfig, LinkMeasurement};

/// A measurement before the join counts this long (#1959): a phone that walked out of the Wi-Fi
/// in the meantime is on another link. Where the platform can't tell networks apart (a browser),
/// the age is all there is.
pub const PRIOR_LINK_MAX_AGE: Duration = Duration::from_secs(120);
/// A probe session the SFU didn't finish within this ends on the client's side.
pub const LINK_PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// What a measurement before the join showed of the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriorLink {
  pub uplink: PriorUplink,
  /// What the SFU's probe of the downlink showed; `None` when it said nothing.
  pub maybe_downlink: Option<LinkMeasurement>,
  pub measured_at: Instant,
}

/// The uplink as the measurement before the join showed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PriorUplink {
  Measured(LinkMeasurement),
  /// The link lost the probe's trains or answered them too late: it is thin
  /// ([`JoinProbe::is_lost_on_the_way`]).
  Lost,
  /// Nothing to go by (no feedback came): the join probes as before.
  Unknown,
}

/// The client's side of a link probe session.
#[derive(Debug)]
pub struct LinkProbeClient {
  uplink: JoinProbe,
  next_seq: u16,
  /// Arrivals of the SFU's padding, reported back so the SFU measures the downlink.
  downlink: FeedbackRecorder,
  maybe_downlink: Option<LinkMeasurement>,
  started_at: Instant,
}

impl PriorLink {
  /// Whether it still counts at `now`.
  pub fn is_fresh(&self, now: Instant) -> bool {
    now.saturating_duration_since(self.measured_at) <= PRIOR_LINK_MAX_AGE
  }
}

impl LinkProbeClient {
  pub fn new(now: Instant) -> Self {
    Self {
      uplink: JoinProbe::new(JoinProbeConfig::default(), now),
      next_seq: 0,
      downlink: FeedbackRecorder::new(now),
      maybe_downlink: None,
      started_at: now,
    }
  }

  /// The uplink's padding datagrams due at `now`.
  pub fn due_padding(&mut self, now: Instant) -> Vec<Vec<u8>> {
    // Trains whose feedback didn't come are given up; the result follows from the reports.
    let _ = self.uplink.on_timer(now);
    let Some(sizes) = self.uplink.due_train(now) else {
      return Vec::new();
    };
    let first = TransportSeq(self.next_seq);
    let datagrams = sizes
      .iter()
      .map(|bytes| {
        let datagram = padding_datagram(*bytes, TransportSeq(self.next_seq));
        self.next_seq = self.next_seq.wrapping_add(1);
        datagram
      })
      .collect();
    self.uplink.sent(first, &sizes, now);
    datagrams
  }

  /// A datagram from the SFU: its downlink padding, reported with the next feedback.
  pub fn on_datagram(&mut self, datagram: &[u8], now: Instant) {
    let Ok(header) = DatagramHeader::parse(datagram) else {
      return;
    };
    if header.stream_id == PADDING_STREAM
      && let Some(seq) = header.maybe_transport_seq
    {
      self.downlink.on_arrival(seq, now);
    }
  }

  /// Feedback on the SFU's padding to send at `now`, if any arrived.
  pub fn take_feedback(&mut self, now: Instant) -> Option<TransportFeedback> {
    self.downlink.take_feedback(now)
  }

  /// An event of the SFU's probe session.
  pub fn on_event(&mut self, event: &CallEvent) {
    match event {
      CallEvent::TransportFeedback { feedback } => {
        let _ = self.uplink.on_feedback(feedback);
      },
      CallEvent::DownlinkMeasured {
        capacity_bps,
        loss_percent,
      } => {
        self.maybe_downlink = Some(LinkMeasurement {
          capacity_bps: *capacity_bps,
          after_burst_bps: *capacity_bps,
          loss_percent: *loss_percent,
        });
      },
      _ => {},
    }
  }

  /// Both ways measured, or the time is up.
  pub fn is_done(&self, now: Instant) -> bool {
    (self.uplink.is_over() && self.maybe_downlink.is_some())
      || now.saturating_duration_since(self.started_at) >= LINK_PROBE_TIMEOUT
  }

  /// What it showed at `now`.
  pub fn result(&self, now: Instant) -> PriorLink {
    let uplink = match self.uplink.result() {
      Some(measurement) => PriorUplink::Measured(measurement),
      None if self.uplink.is_lost_on_the_way() => PriorUplink::Lost,
      None => PriorUplink::Unknown,
    };
    PriorLink {
      uplink,
      maybe_downlink: self.maybe_downlink,
      measured_at: now,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// A good link: three trains a moment apart, each answered spread as 2 Mbit/s spreads them, and
  /// the SFU's downlink result.
  #[test]
  fn a_good_link_is_measured_both_ways() {
    let start = Instant::now();
    let mut probe = LinkProbeClient::new(start);
    let mut at = start;
    let mut seq = 0u16;
    for _ in 0..3 {
      let padding = probe.due_padding(at);
      assert_eq!(padding.len(), 5);
      let feedback = TransportFeedback {
        base_seq: seq,
        arrivals_us: (0..5).map(|index| Some(index * 4000)).collect(),
      };
      probe.on_event(&CallEvent::TransportFeedback { feedback });
      seq += 5;
      at += Duration::from_millis(150);
    }
    assert!(!probe.is_done(at), "the downlink is still to come");
    probe.on_event(&CallEvent::DownlinkMeasured {
      capacity_bps: 9_000_000,
      loss_percent: 0,
    });
    assert!(probe.is_done(at));
    let result = probe.result(at);
    let PriorUplink::Measured(uplink) = result.uplink else {
      panic!("{result:?}");
    };
    assert_eq!(uplink.capacity_bps, 2_000_000);
    assert_eq!(result.maybe_downlink.unwrap().capacity_bps, 9_000_000);
    assert!(result.is_fresh(at + PRIOR_LINK_MAX_AGE));
    assert!(!result.is_fresh(at + PRIOR_LINK_MAX_AGE + Duration::from_secs(1)));
  }

  /// A thin link that loses the trains: lost, which the call takes as thin.
  #[test]
  fn a_thin_link_that_loses_the_trains_is_lost() {
    let start = Instant::now();
    let mut probe = LinkProbeClient::new(start);
    let mut at = start;
    let mut seq = 0u16;
    for _ in 0..3 {
      probe.due_padding(at);
      let feedback = TransportFeedback {
        base_seq: seq,
        arrivals_us: vec![Some(0), None, None, Some(250_000), None],
      };
      probe.on_event(&CallEvent::TransportFeedback { feedback });
      seq += 5;
      at += Duration::from_millis(150);
    }
    assert_eq!(probe.result(at).uplink, PriorUplink::Lost);
    assert!(
      probe.is_done(start + LINK_PROBE_TIMEOUT),
      "times out without a downlink result"
    );
  }

  /// The SFU's padding is reported back; anything else is not.
  #[test]
  fn the_sfus_padding_is_reported_back() {
    let start = Instant::now();
    let mut probe = LinkProbeClient::new(start);
    for seq in 0..3u16 {
      probe.on_datagram(&padding_datagram(1000, TransportSeq(seq)), start);
    }
    probe.on_datagram(&[1, 2, 3], start);
    let feedback = probe.take_feedback(start + Duration::from_millis(200)).unwrap();
    assert_eq!(feedback.arrivals_us.iter().flatten().count(), 3);
    // Silence from the SFU: nothing to go by.
    let quiet = LinkProbeClient::new(start);
    assert_eq!(quiet.result(start).uplink, PriorUplink::Unknown);
  }
}
