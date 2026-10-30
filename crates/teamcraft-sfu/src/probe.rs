//! Probing a receiver's downlink with padding before a layer switch up (#994).
//!
//! The downlink estimate grows while the link isn't full (the sender declares the demand of the
//! layers the receiver asked for), and the next simulcast layer used to be the probe: a viewer
//! whose link carried only 180p was switched up to 360p now and then, the link dropped it, and
//! the picture stood still for up to 5 s until the SFU fell back.
//!
//! Now the switch up waits for a padding burst to show that the link carries the new rate: a few
//! datagrams on [`PADDING_STREAM`], sent back to back, numbered for the receiver's transport
//! feedback like any other. The bottleneck spaces them out at its rate; the receiver reports their
//! arrival times, and their spread tells what the link carries. A burst of a few kilobytes queues
//! for a moment on a link that can't take the rate, but drops nothing and holds up no picture. A
//! burst that falls short limits the estimate to what got through (it grows again from there,
//! and the next burst comes when it covers the layer again); one that gets through raises it
//! there and lets the allocation switch up.
//!
//! Receivers drop datagrams of streams they don't know, and report every datagram's arrival, so
//! clients from before the probes take part unchanged.

use std::time::{Duration, Instant};

use teamcraft_media::datagram::{DatagramHeader, PADDING_STREAM, SimulcastLayer, TransportSeq};
use teamcraft_proto::TransportFeedback;

/// Settings of the downlink probes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeConfig {
  /// A burst sends what its target rate sends in this time ...
  pub burst: Duration,
  /// ... in datagrams of this size (large enough that QUIC sends each in a packet of its own) ...
  pub datagram_bytes: usize,
  /// ... and at least this many of them (their spread has to stand out from the timing noise
  /// of the receiver), at most this many.
  pub min_datagrams: usize,
  pub max_datagrams: usize,
  /// How long what a burst showed counts for switching up.
  pub result_valid_for: Duration,
  /// The wait after a burst that fell short before the next one; it doubles with every further
  /// one up to `max_retry_after`, and a burst that gets through ends it.
  pub retry_after: Duration,
  pub max_retry_after: Duration,
  /// A burst whose feedback hasn't come within this is given up.
  pub timeout: Duration,
}

/// The probes of one receiver's downlink.
#[derive(Debug, Default)]
pub struct DownlinkProbe {
  maybe_burst: Option<Burst>,
  /// The rate the last burst showed the link carries, and when.
  maybe_shown: Option<(u32, Instant)>,
  maybe_next_at: Option<Instant>,
  /// The wait after the next burst that falls short; zero before the first.
  retry_wait: Duration,
}

/// What a burst showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeOutcome {
  /// The link carried the target rate; it carries `measured_bps` (at most twice the target).
  Carried { measured_bps: u32 },
  /// The link carries less than the target: `measured_bps` (0 when too little got through to
  /// tell). `floor_bps` keeps what the receiver gets now.
  FellShort { measured_bps: u32, floor_bps: u32 },
}

/// A burst in flight.
#[derive(Debug)]
struct Burst {
  target_bps: u32,
  floor_bps: u32,
  sent_at: Instant,
  first_seq: TransportSeq,
  /// Per datagram: its bytes, and what the feedback said of it.
  datagrams: Vec<(usize, Report)>,
}

/// What the receiver's feedback said of one datagram of a burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Report {
  Pending,
  Lost,
  /// Arrival time in µs (receiver's clock).
  Arrived(u64),
}

impl Default for ProbeConfig {
  fn default() -> Self {
    Self {
      burst: Duration::from_millis(50),
      datagram_bytes: 1000,
      min_datagrams: 4,
      max_datagrams: 40,
      result_valid_for: Duration::from_secs(3),
      retry_after: Duration::from_secs(2),
      max_retry_after: Duration::from_secs(4),
      timeout: Duration::from_secs(1),
    }
  }
}

impl DownlinkProbe {
  pub fn new() -> Self {
    Self::default()
  }

  /// The rate a recent burst showed the link carries; 0 without one.
  pub fn shown_bps(&self, config: &ProbeConfig, now: Instant) -> u32 {
    self
      .maybe_shown
      .filter(|(_, at)| now.saturating_duration_since(*at) <= config.result_valid_for)
      .map_or(0, |(bps, _)| bps)
  }

  /// Whether a burst may go out now: none in flight, and the last one long enough ago.
  pub fn may_start(&self, now: Instant) -> bool {
    self.maybe_burst.is_none() && self.maybe_next_at.is_none_or(|next| now >= next)
  }

  /// The sizes of the datagrams of a burst at `target_bps`.
  pub fn burst_sizes(config: &ProbeConfig, target_bps: u32) -> Vec<usize> {
    let bytes = f64::from(target_bps) / 8.0 * config.burst.as_secs_f64();
    // Saturating float-to-int conversion is intended.
    let count = ((bytes / config.datagram_bytes as f64).ceil() as usize)
      .clamp(config.min_datagrams, config.max_datagrams);
    vec![config.datagram_bytes; count]
  }

  /// A burst at `target_bps` went out at `now` as consecutive transport sequence numbers from
  /// `first_seq`, of `sizes` bytes.
  pub fn started(
    &mut self,
    target_bps: u32,
    floor_bps: u32,
    first_seq: TransportSeq,
    sizes: &[usize],
    now: Instant,
  ) {
    self.maybe_burst = Some(Burst {
      target_bps,
      floor_bps,
      sent_at: now,
      first_seq,
      datagrams: sizes.iter().map(|bytes| (*bytes, Report::Pending)).collect(),
    });
  }

  /// Notes the arrivals of a burst's datagrams; once all are reported, what it showed.
  pub fn on_feedback(
    &mut self,
    config: &ProbeConfig,
    feedback: &TransportFeedback,
    now: Instant,
  ) -> Option<ProbeOutcome> {
    let burst = self.maybe_burst.as_mut()?;
    for (offset, maybe_arrival_us) in feedback.arrivals_us.iter().enumerate() {
      // Feedback numbers are 16 bits wide like the sequence numbers; a burst is a few of them.
      let seq = feedback.base_seq.wrapping_add(offset as u16);
      let index = usize::from(seq.wrapping_sub(burst.first_seq.0));
      if let Some((_, report)) = burst.datagrams.get_mut(index) {
        *report = maybe_arrival_us.map_or(Report::Lost, Report::Arrived);
      }
    }
    if burst
      .datagrams
      .iter()
      .any(|(_, report)| *report == Report::Pending)
    {
      return None;
    }
    let outcome = burst.outcome();
    self.maybe_burst = None;
    match outcome {
      ProbeOutcome::Carried { measured_bps } => {
        self.maybe_shown = Some((measured_bps, now));
        self.maybe_next_at = None;
        self.retry_wait = Duration::ZERO;
      },
      ProbeOutcome::FellShort { .. } => {
        self.retry_wait = (self.retry_wait * 2).clamp(config.retry_after, config.max_retry_after);
        self.maybe_next_at = Some(now + self.retry_wait);
      },
    }
    Some(outcome)
  }

  /// Gives up a burst whose feedback doesn't come (the receiver lost it, or reports none).
  pub fn on_timer(&mut self, config: &ProbeConfig, now: Instant) {
    if self
      .maybe_burst
      .as_ref()
      .is_some_and(|burst| now.saturating_duration_since(burst.sent_at) >= config.timeout)
    {
      self.maybe_burst = None;
      self.maybe_next_at = Some(now + config.retry_after);
    }
  }
}

impl Burst {
  /// The rate the arrivals' spread shows: the bytes after the first arrival over the time they
  /// took. Any loss means the link's queue overflowed: short of the target, whatever the spread.
  fn outcome(&self) -> ProbeOutcome {
    let mut arrived: Vec<(u64, usize)> = self
      .datagrams
      .iter()
      .filter_map(|(bytes, report)| match report {
        Report::Arrived(arrival_us) => Some((*arrival_us, *bytes)),
        Report::Pending | Report::Lost => None,
      })
      .collect();
    arrived.sort_unstable();
    let is_complete = arrived.len() == self.datagrams.len();
    let measured_bps = match (arrived.first(), arrived.last()) {
      (Some((first_us, _)), Some((last_us, _))) if arrived.len() >= 2 => {
        let bits = arrived.iter().skip(1).map(|(_, bytes)| *bytes).sum::<usize>() as f64 * 8.0;
        let span_us = last_us.saturating_sub(*first_us);
        if span_us == 0 {
          u32::MAX
        } else {
          // Saturating float-to-int conversion is intended.
          (bits * 1_000_000.0 / span_us as f64) as u32
        }
      },
      _ => 0,
    };
    if is_complete && measured_bps >= self.target_bps {
      // The spread of datagrams sent back to back shows the link's capacity rather than what
      // is free of it: a burst shows at most twice what it was sent for.
      ProbeOutcome::Carried {
        measured_bps: measured_bps.min(self.target_bps.saturating_mul(2)),
      }
    } else {
      ProbeOutcome::FellShort {
        measured_bps,
        floor_bps: self.floor_bps,
      }
    }
  }
}

/// A padding datagram of `bytes` bytes with transport sequence number `seq`.
pub fn padding_datagram(bytes: usize, seq: TransportSeq) -> Vec<u8> {
  let header = DatagramHeader {
    stream_id: PADDING_STREAM,
    layer: SimulcastLayer::BASE,
    maybe_transport_seq: Some(seq),
  };
  let filler = vec![0u8; bytes.saturating_sub(header.encoded_len())];
  let mut out = Vec::with_capacity(bytes);
  header.write_to(&mut out);
  out.extend_from_slice(&filler);
  out
}

#[cfg(test)]
mod tests {
  use teamcraft_media::datagram::MediaDatagram;

  use super::*;

  #[test]
  fn a_burst_that_gets_through_at_the_target_rate_carries_it() {
    let config = ProbeConfig::default();
    let mut probe = DownlinkProbe::new();
    let now = Instant::now();
    let sizes = DownlinkProbe::burst_sizes(&config, 1_000_000);
    // 1 Mbit/s for 50 ms: 6250 bytes, in seven datagrams of 1000.
    assert_eq!(sizes.len(), 7);
    probe.started(1_000_000, 400_000, TransportSeq(65_534), &sizes, now);
    assert!(!probe.may_start(now));
    // Spaced as a 1.6 Mbit/s link spaces them: 5 ms apart (the numbers wrap).
    let feedback = TransportFeedback {
      base_seq: 65_533,
      arrivals_us: std::iter::once(Some(0))
        .chain((0..7).map(|index| Some(10_000 + index * 5_000)))
        .collect(),
    };
    let outcome = probe.on_feedback(&config, &feedback, now).unwrap();
    assert_eq!(
      outcome,
      ProbeOutcome::Carried {
        measured_bps: 1_600_000
      }
    );
    assert_eq!(probe.shown_bps(&config, now), 1_600_000);
    assert_eq!(probe.shown_bps(&config, now + Duration::from_secs(4)), 0);
    assert!(probe.may_start(now));
  }

  /// Bursts that fall short come 2, 4, 4 s apart; one that gets through ends the wait.
  #[test]
  fn bursts_that_fall_short_back_off() {
    let config = ProbeConfig::default();
    let mut probe = DownlinkProbe::new();
    let mut now = Instant::now();
    let mut waits = Vec::new();
    for burst in 0..4u16 {
      probe.started(550_000, 230_000, TransportSeq(burst * 4), &[1000; 4], now);
      let slow = TransportFeedback {
        base_seq: burst * 4,
        arrivals_us: vec![Some(0), Some(32_000), Some(64_000), Some(96_000)],
      };
      probe.on_feedback(&config, &slow, now).unwrap();
      let wait = (1..=20)
        .map(Duration::from_secs)
        .find(|wait| probe.may_start(now + *wait))
        .unwrap();
      waits.push(wait.as_secs());
      now += wait;
    }
    assert_eq!(waits, vec![2, 4, 4, 4]);
    probe.started(550_000, 230_000, TransportSeq(16), &[1000; 4], now);
    let fast = TransportFeedback {
      base_seq: 16,
      arrivals_us: vec![Some(0), Some(1_000), Some(2_000), Some(3_000)],
    };
    // 8 Mbit/s, shown as twice the target.
    assert_eq!(
      probe.on_feedback(&config, &fast, now),
      Some(ProbeOutcome::Carried {
        measured_bps: 1_100_000
      })
    );
    assert!(probe.may_start(now));
  }

  #[test]
  fn a_burst_spread_by_a_slow_link_falls_short() {
    let config = ProbeConfig::default();
    let mut probe = DownlinkProbe::new();
    let now = Instant::now();
    let sizes = DownlinkProbe::burst_sizes(&config, 550_000);
    assert_eq!(sizes.len(), 4);
    probe.started(550_000, 230_000, TransportSeq(10), &sizes, now);
    // 250 kbit/s: 32 ms per datagram; reported in two pieces.
    let first = TransportFeedback {
      base_seq: 10,
      arrivals_us: vec![Some(0), Some(32_000)],
    };
    assert_eq!(probe.on_feedback(&config, &first, now), None);
    let second = TransportFeedback {
      base_seq: 12,
      arrivals_us: vec![Some(64_000), Some(96_000)],
    };
    assert_eq!(
      probe.on_feedback(&config, &second, now),
      Some(ProbeOutcome::FellShort {
        measured_bps: 250_000,
        floor_bps: 230_000
      })
    );
    assert_eq!(probe.shown_bps(&config, now), 0);
  }

  #[test]
  fn a_lost_datagram_falls_short() {
    let config = ProbeConfig::default();
    let mut probe = DownlinkProbe::new();
    let now = Instant::now();
    probe.started(550_000, 230_000, TransportSeq(0), &[1000; 4], now);
    let feedback = TransportFeedback {
      base_seq: 0,
      arrivals_us: vec![Some(0), Some(1_000), None, Some(2_000)],
    };
    assert!(matches!(
      probe.on_feedback(&config, &feedback, now),
      Some(ProbeOutcome::FellShort { .. })
    ));
  }

  #[test]
  fn a_burst_without_feedback_is_given_up() {
    let config = ProbeConfig::default();
    let mut probe = DownlinkProbe::new();
    let now = Instant::now();
    probe.started(550_000, 230_000, TransportSeq(0), &[1000; 4], now);
    probe.on_timer(&config, now + Duration::from_millis(900));
    assert!(!probe.may_start(now + Duration::from_millis(900)));
    probe.on_timer(&config, now + Duration::from_secs(1));
    assert!(probe.may_start(now + Duration::from_secs(3)));
  }

  #[test]
  fn padding_parses_as_a_datagram_of_the_padding_stream() {
    let datagram = padding_datagram(1000, TransportSeq(7));
    assert_eq!(datagram.len(), 1000);
    let parsed = MediaDatagram::parse(&datagram).unwrap();
    assert_eq!(parsed.header.stream_id, PADDING_STREAM);
    assert_eq!(parsed.header.maybe_transport_seq, Some(TransportSeq(7)));
  }
}
