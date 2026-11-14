//! Measuring a device's link before it joins anything (#1959): the connection of a link probe
//! ticket ([`teamcraft_proto::CallClientMessage::Probe`]).
//!
//! The client pads its uplink as at the join (`teamcraft_media::join_probe`); the SFU answers with
//! transport feedback, so the client measures its uplink, and probes the downlink with padding of
//! its own, reporting what it showed ([`teamcraft_proto::CallEvent::DownlinkMeasured`]). No room, no
//! participant, no roster: nobody learns that the device measures.
//!
//! Bounded on every side: padding up to the probe budget each way (`probe_bytes`, as at the join),
//! a session lasts at most [`LinkProbeConfig::max_length`], and a ticket opens one session
//! ([`ProbeTickets`]). Padding goes only to a client that showed a valid ticket on a QUIC
//! connection whose handshake validated its address, so it can't be turned against a third party.
//!
//! Pure: time is passed in.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use teamcraft_media::{
  bwe::FeedbackRecorder,
  datagram::{DatagramError, DatagramHeader, PADDING_STREAM, TransportSeq, padding_datagram},
  join_probe::{JoinProbe, JoinProbeConfig, LinkMeasurement},
};
use teamcraft_proto::TransportFeedback;
use thiserror::Error;

/// Settings of the link probe sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkProbeConfig {
  /// The padding budget each way, as for the probes at the join (`ForwardingConfig::probe_bytes`).
  /// Zero turns link probe sessions off.
  pub probe_bytes: u32,
  /// A session ends this long after it began, whatever it measured.
  pub max_length: Duration,
  /// Ticket ids remembered at once; a ticket that finds the memory full is refused.
  pub max_remembered_tickets: usize,
}

/// One device's link probe session.
#[derive(Debug)]
pub struct LinkProbe {
  config: LinkProbeConfig,
  started_at: Instant,
  /// Arrivals of the client's padding, for its uplink.
  uplink: FeedbackRecorder,
  /// Padding bytes of the client taken so far: beyond twice the budget, the rest is dropped.
  uplink_bytes: usize,
  /// The probe of the client's downlink; `None` once over or with no budget.
  maybe_downlink: Option<JoinProbe>,
  next_seq: u16,
  is_measured: bool,
}

/// What a timer tick asks of the session's connection.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct LinkProbeTick {
  /// Feedback on the client's padding, to send.
  pub maybe_feedback: Option<TransportFeedback>,
  /// Padding datagrams of the downlink probe, to send.
  pub padding: Vec<Vec<u8>>,
  /// The downlink's measurement, once, when it ended by timeout.
  pub maybe_measured: Option<LinkMeasurement>,
  /// The session's time is up: close it.
  pub is_over: bool,
}

/// Ticket ids the SFU took, until they expire (#1959): each ticket opens one session.
#[derive(Debug, Default)]
pub struct ProbeTickets {
  /// Ticket id → expiry, Unix seconds.
  taken: HashMap<String, u64>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LinkProbeError {
  #[error(transparent)]
  Datagram(#[from] DatagramError),
  #[error("only padding is taken on a link probe session")]
  NotPadding,
}

impl LinkProbe {
  pub fn new(config: LinkProbeConfig, now: Instant) -> Self {
    let maybe_downlink = JoinProbeConfig::default()
      .within_bytes(usize::try_from(config.probe_bytes).unwrap_or(usize::MAX))
      .map(|probe_config| JoinProbe::new(probe_config, now));
    Self {
      config,
      started_at: now,
      uplink: FeedbackRecorder::new(now),
      uplink_bytes: 0,
      maybe_downlink,
      next_seq: 0,
      is_measured: false,
    }
  }

  /// A datagram of the client: its uplink's padding, reported with the next feedback.
  pub fn on_datagram(&mut self, datagram: &[u8], now: Instant) -> Result<(), LinkProbeError> {
    let header = DatagramHeader::parse(datagram)?;
    if header.stream_id != PADDING_STREAM {
      return Err(LinkProbeError::NotPadding);
    }
    let budget = usize::try_from(self.config.probe_bytes)
      .unwrap_or(usize::MAX)
      .saturating_mul(2);
    if self.uplink_bytes >= budget {
      return Ok(());
    }
    self.uplink_bytes += datagram.len();
    if let Some(seq) = header.maybe_transport_seq {
      self.uplink.on_arrival(seq, now);
    }
    Ok(())
  }

  /// The client's feedback on the downlink's padding: the measurement, once, when it completes.
  pub fn on_feedback(&mut self, feedback: &TransportFeedback) -> Option<LinkMeasurement> {
    let measurement = self.maybe_downlink.as_mut()?.on_feedback(feedback)?;
    self.is_measured = true;
    Some(measurement)
  }

  /// Runs every timer tick (~100 ms): feedback, the due padding train, the end.
  pub fn on_timer(&mut self, now: Instant) -> LinkProbeTick {
    let mut tick = LinkProbeTick {
      maybe_feedback: self.uplink.take_feedback(now),
      is_over: now.saturating_duration_since(self.started_at) >= self.config.max_length,
      ..LinkProbeTick::default()
    };
    if tick.is_over {
      return tick;
    }
    let Some(probe) = self.maybe_downlink.as_mut() else {
      return tick;
    };
    if let Some(measurement) = probe.on_timer(now) {
      if !self.is_measured {
        self.is_measured = true;
        tick.maybe_measured = Some(measurement);
      }
      return tick;
    }
    let Some(sizes) = probe.due_train(now) else {
      return tick;
    };
    let first_seq = TransportSeq(self.next_seq);
    for bytes in &sizes {
      tick
        .padding
        .push(padding_datagram(*bytes, TransportSeq(self.next_seq)));
      self.next_seq = self.next_seq.wrapping_add(1);
    }
    probe.sent(first_seq, &sizes, now);
    tick
  }
}

impl ProbeTickets {
  /// Takes the ticket `id` (expiring at `expires_at_unix_secs`) for a session at `now_unix_secs`:
  /// `false` when it was taken before, or the memory is full.
  pub fn take(
    &mut self,
    id: &str,
    expires_at_unix_secs: u64,
    now_unix_secs: u64,
    max_remembered: usize,
  ) -> bool {
    self.taken.retain(|_, expires_at| *expires_at >= now_unix_secs);
    if self.taken.contains_key(id) || self.taken.len() >= max_remembered {
      return false;
    }
    self.taken.insert(id.to_owned(), expires_at_unix_secs);
    true
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  const CONFIG: LinkProbeConfig = LinkProbeConfig {
    probe_bytes: 15_000,
    max_length: Duration::from_secs(10),
    max_remembered_tickets: 4,
  };

  mod session {
    use super::*;

    /// The client's padding is answered with feedback; anything but padding is refused.
    #[test]
    fn padding_is_reported_and_media_refused() {
      let start = Instant::now();
      let mut probe = LinkProbe::new(CONFIG, start);
      for seq in 0..5u16 {
        probe
          .on_datagram(&padding_datagram(1000, TransportSeq(seq)), start)
          .unwrap();
      }
      let tick = probe.on_timer(start + Duration::from_millis(200));
      let feedback = tick.maybe_feedback.expect("feedback");
      assert_eq!(feedback.arrivals_us.iter().flatten().count(), 5);
      let mut media = padding_datagram(1000, TransportSeq(9));
      // Another stream than padding: the stream id follows the header's first byte.
      media[4] ^= 0x01;
      assert!(probe.on_datagram(&media, start).is_err());
      assert!(probe.on_datagram(&[1, 2], start).is_err(), "garbage");
    }

    /// The downlink is probed with trains a moment apart, and the client's feedback measures it.
    #[test]
    fn the_downlink_is_measured_from_the_clients_feedback() {
      let start = Instant::now();
      let mut probe = LinkProbe::new(CONFIG, start);
      let mut at = start;
      let mut seq = 0u16;
      let mut maybe_result = None;
      for _ in 0..3 {
        let tick = probe.on_timer(at);
        assert_eq!(tick.padding.len(), 5, "a train");
        // 5 datagrams of 1000 bytes, 4 ms apart: 2 Mbit/s.
        let feedback = TransportFeedback {
          base_seq: seq,
          arrivals_us: (0..5).map(|index| Some(index * 4000)).collect(),
        };
        maybe_result = probe.on_feedback(&feedback).or(maybe_result);
        seq += 5;
        at += Duration::from_millis(150);
      }
      assert_eq!(maybe_result.unwrap().capacity_bps, 2_000_000);
      assert!(probe.on_timer(at).padding.is_empty(), "no more trains");
    }

    /// The padding budget bounds both ways; a session ends at its length.
    #[test]
    fn padding_and_time_are_bounded() {
      let start = Instant::now();
      let mut probe = LinkProbe::new(CONFIG, start);
      let mut sent = 0;
      let mut at = start;
      while at < start + CONFIG.max_length {
        sent += probe.on_timer(at).padding.iter().map(Vec::len).sum::<usize>();
        at += Duration::from_millis(100);
      }
      assert!(sent <= 15_000 + 1000, "{sent}");
      assert!(probe.on_timer(start + CONFIG.max_length).is_over);
      // The client's padding beyond twice the budget isn't recorded.
      for seq in 0..40u16 {
        probe
          .on_datagram(&padding_datagram(1000, TransportSeq(seq)), start)
          .unwrap();
      }
      assert!(probe.uplink_bytes <= 2 * 15_000 + 1100);
      // No budget, no downlink padding.
      let mut off = LinkProbe::new(
        LinkProbeConfig {
          probe_bytes: 0,
          ..CONFIG
        },
        start,
      );
      assert!(off.on_timer(start).padding.is_empty());
    }
  }

  mod tickets {
    use super::*;

    #[test]
    fn a_ticket_opens_one_session() {
      let mut tickets = ProbeTickets::default();
      assert!(tickets.take("a", 130, 100, 4));
      assert!(!tickets.take("a", 130, 101, 4), "used");
      assert!(tickets.take("b", 130, 101, 4));
    }

    #[test]
    fn expired_ids_are_forgotten_and_the_memory_is_bounded() {
      let mut tickets = ProbeTickets::default();
      for id in ["a", "b", "c", "d"] {
        assert!(tickets.take(id, 130, 100, 4));
      }
      assert!(!tickets.take("e", 130, 100, 4), "full");
      // Once the first ones expired, there is room again; an expired ticket wouldn't verify anyway.
      assert!(tickets.take("e", 200, 131, 4));
    }
  }
}
