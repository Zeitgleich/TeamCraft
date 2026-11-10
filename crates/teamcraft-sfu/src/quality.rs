//! How calls go, in aggregates over every participant (#1648): round-trip time and loss of the
//! SFU's QUIC paths, keyframe requests, bytes forwarded, and which transport and address family
//! sessions reach the SFU on.
//!
//! Aggregates only: no call, user, device or address in a label, so the numbers say nothing about
//! who talks to whom (ARCHITECTURE.md §2). Memory is fixed: a handful of counters and two
//! histograms with fixed buckets, whatever the number of calls.
//!
//! Cost on the media path: forwarding a datagram adds one relaxed atomic add (the bytes it went out
//! with), a keyframe request one more. The path statistics are read from QUIC once per
//! [`PATH_SAMPLE_INTERVAL`] per session, beside the session's other timers.
//!
//! Calls run on QUIC datagrams, not RTP, so there are no RTCP receiver reports: loss and RTT come
//! from the QUIC connection, which acknowledges every packet (loss of the SFU's packets to the
//! client, i.e. the downlink). Media is never retransmitted, so there are no NACKs to count; a
//! keyframe request is the SFU's PLI.

use std::net::IpAddr;
use std::time::Duration;

use teamcraft_ops::metrics::{
  Histogram, HistogramOpts, IntCounter, IntCounterVec, Metrics, MetricsError, Opts,
};
use wtransport::Connection;

/// How often a session's QUIC path statistics go into the histograms.
pub const PATH_SAMPLE_INTERVAL: Duration = Duration::from_secs(10);

/// A sample's loss counts only when the SFU sent at least this many packets in it: a path that
/// carried a few keep-alives says nothing about loss, and one lost packet of them would read as a
/// disaster.
const MIN_PACKETS_FOR_LOSS: u64 = 100;

/// Buckets of the round-trip time, in seconds: from a LAN to a bad mobile link.
const RTT_BUCKETS_SECS: [f64; 10] = [0.01, 0.025, 0.05, 0.1, 0.15, 0.2, 0.3, 0.5, 1.0, 2.0];

/// Buckets of the loss ratio: about 2 % is where audio starts to suffer, 10 % where video does.
const LOSS_BUCKETS: [f64; 8] = [0.001, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5];

/// The aggregates. Clones of the prometheus values share their counts, so the metrics registry
/// reads what the sessions write; with metrics off they are counted and never read.
pub struct CallQuality {
  rtt_seconds: Histogram,
  downlink_loss_ratio: Histogram,
  keyframe_requests: IntCounter,
  forwarded_bytes: IntCounter,
  sessions: IntCounterVec,
  session_switches: IntCounter,
}

/// What a session reaches the SFU on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
  Quic,
  /// The WebSocket fallback, for networks that block UDP.
  Fallback,
}

/// The address family a session's client reached the SFU from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressFamily {
  Ipv4,
  Ipv6,
}

/// The counters of a QUIC path at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PathCounters {
  pub sent_packets: u64,
  pub lost_packets: u64,
}

impl CallQuality {
  pub fn new() -> Self {
    // Invariant: the names, help texts, labels and buckets are the constants below, valid for
    // prometheus; `it_holds_the_counts_and_nothing_else` in metrics.rs builds and renders them.
    #[allow(clippy::expect_used)]
    Self::build().expect("the call quality metrics are well-formed")
  }

  /// Adds every family to `metrics`, so its endpoint shows them.
  pub fn register(&self, metrics: &Metrics) -> Result<(), MetricsError> {
    metrics.register(&self.rtt_seconds)?;
    metrics.register(&self.downlink_loss_ratio)?;
    metrics.register(&self.keyframe_requests)?;
    metrics.register(&self.forwarded_bytes)?;
    metrics.register(&self.sessions)?;
    metrics.register(&self.session_switches)
  }

  /// A session joined a call.
  pub fn session_joined(&self, transport: Transport, ip: IpAddr) {
    let family = AddressFamily::of(ip);
    self
      .sessions
      .with_label_values(&[transport.label(), family.label()])
      .inc();
  }

  /// A running session moved onto a new QUIC connection.
  pub fn session_switched(&self) {
    self.session_switches.inc();
  }

  /// The SFU asked a publisher for a keyframe.
  pub fn keyframe_requested(&self) {
    self.keyframe_requests.inc();
  }

  /// One datagram went out to receivers, `bytes` in all.
  pub fn forwarded(&self, bytes: usize) {
    self
      .forwarded_bytes
      .inc_by(u64::try_from(bytes).unwrap_or(u64::MAX));
  }

  /// One sample of a QUIC path: its round-trip time, and its loss since `previous`.
  pub fn record_path(&self, rtt: Duration, previous: PathCounters, now: PathCounters) {
    self.rtt_seconds.observe(rtt.as_secs_f64());
    if let Some(ratio) = loss_ratio(previous, now) {
      self.downlink_loss_ratio.observe(ratio);
    }
  }

  fn build() -> Result<Self, MetricsError> {
    let sessions = IntCounterVec::new(
      Opts::new(
        "teamcraft_sfu_sessions_total",
        "Sessions that joined a call, by transport (quic, fallback) and address family (ipv4, ipv6).",
      ),
      &["transport", "family"],
    )?;
    // Every series from the start: a rate over a series that appears late misses its first count.
    for transport in [Transport::Quic, Transport::Fallback] {
      for family in [AddressFamily::Ipv4, AddressFamily::Ipv6] {
        sessions.with_label_values(&[transport.label(), family.label()]);
      }
    }
    Ok(Self {
      rtt_seconds: Histogram::with_opts(
        HistogramOpts::new(
          "teamcraft_sfu_rtt_seconds",
          "Round-trip time of the SFU's QUIC paths, one sample per session every 10 s.",
        )
        .buckets(RTT_BUCKETS_SECS.to_vec()),
      )?,
      downlink_loss_ratio: Histogram::with_opts(
        HistogramOpts::new(
          "teamcraft_sfu_downlink_loss_ratio",
          "Share of the SFU's QUIC packets to a client that were lost, one sample per busy session every 10 s.",
        )
        .buckets(LOSS_BUCKETS.to_vec()),
      )?,
      keyframe_requests: IntCounter::new(
        "teamcraft_sfu_keyframe_requests_total",
        "Keyframes the SFU asked publishers for (a receiver switched layers or lost the picture).",
      )?,
      forwarded_bytes: IntCounter::new(
        "teamcraft_sfu_forwarded_bytes_total",
        "Bytes of media datagrams the SFU forwarded to receivers.",
      )?,
      session_switches: IntCounter::new(
        "teamcraft_sfu_session_switches_total",
        "Running sessions that moved onto a new QUIC connection, mostly from the WebSocket fallback once UDP got through.",
      )?,
      sessions,
    })
  }
}

impl Default for CallQuality {
  fn default() -> Self {
    Self::new()
  }
}

impl Transport {
  fn label(self) -> &'static str {
    match self {
      Self::Quic => "quic",
      Self::Fallback => "fallback",
    }
  }
}

impl AddressFamily {
  /// The family of `ip`; an IPv4-mapped IPv6 address (a dual-stack socket's IPv4 peer) is IPv4.
  pub fn of(ip: IpAddr) -> Self {
    match ip.to_canonical() {
      IpAddr::V4(_) => Self::Ipv4,
      IpAddr::V6(_) => Self::Ipv6,
    }
  }

  fn label(self) -> &'static str {
    match self {
      Self::Ipv4 => "ipv4",
      Self::Ipv6 => "ipv6",
    }
  }
}

impl PathCounters {
  /// The counters of `connection`'s path now.
  pub fn of(connection: &Connection) -> Self {
    let stats = connection.quic_connection().stats();
    Self {
      sent_packets: stats.path.sent_packets,
      lost_packets: stats.path.lost_packets,
    }
  }
}

/// Samples `connection`'s path every [`PATH_SAMPLE_INTERVAL`] into `quality`, for as long as it
/// runs (the session drops it when it ends).
pub async fn sample_path(connection: &Connection, quality: &CallQuality) {
  let mut interval = tokio::time::interval(PATH_SAMPLE_INTERVAL);
  // The first tick is at once: it only takes the starting counters.
  interval.tick().await;
  let mut previous = PathCounters::of(connection);
  loop {
    interval.tick().await;
    let now = PathCounters::of(connection);
    quality.record_path(connection.rtt(), previous, now);
    previous = now;
  }
}

/// The share of packets lost between two readings of a path's counters; `None` when too few went
/// out to tell, or the counters make no sense (they never go backwards on one connection).
fn loss_ratio(previous: PathCounters, now: PathCounters) -> Option<f64> {
  let sent = now.sent_packets.checked_sub(previous.sent_packets)?;
  let lost = now.lost_packets.checked_sub(previous.lost_packets)?;
  if sent < MIN_PACKETS_FOR_LOSS {
    return None;
  }
  // Packets sent late in the previous window can be declared lost in this one.
  #[allow(clippy::cast_precision_loss)]
  Some((lost as f64 / sent as f64).min(1.0))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
  use std::net::{Ipv4Addr, Ipv6Addr};

  use super::*;

  mod loss {
    use super::*;

    #[test]
    fn it_is_the_share_lost_since_the_last_reading() {
      let ratio = loss_ratio(counters(1_000, 10), counters(2_000, 30)).unwrap();
      assert!((ratio - 0.02).abs() < 1e-9);
    }

    #[test]
    fn a_quiet_path_says_nothing() {
      assert_eq!(loss_ratio(counters(1_000, 0), counters(1_099, 5)), None);
      assert_eq!(loss_ratio(counters(1_000, 0), counters(1_000, 0)), None);
    }

    #[test]
    fn counters_that_went_backwards_say_nothing() {
      assert_eq!(loss_ratio(counters(2_000, 0), counters(1_000, 0)), None);
      assert_eq!(loss_ratio(counters(1_000, 50), counters(2_000, 10)), None);
    }

    #[test]
    fn late_losses_never_make_more_than_everything() {
      assert_eq!(loss_ratio(counters(0, 0), counters(100, 150)), Some(1.0));
    }

    #[test]
    fn huge_counters_do_not_overflow() {
      let ratio = loss_ratio(counters(u64::MAX - 1_000, 0), counters(u64::MAX, 1)).unwrap();
      assert!((ratio - 0.001).abs() < 1e-9);
    }
  }

  mod aggregates {
    use super::*;

    #[test]
    fn samples_land_in_the_histograms() {
      let quality = CallQuality::new();
      quality.record_path(Duration::from_millis(40), counters(0, 0), counters(1_000, 10));
      quality.record_path(
        Duration::from_millis(400),
        counters(1_000, 10),
        counters(1_010, 10),
      );
      assert_eq!(quality.rtt_seconds.get_sample_count(), 2);
      // The second sample had too few packets for a loss.
      assert_eq!(quality.downlink_loss_ratio.get_sample_count(), 1);
      assert!((quality.downlink_loss_ratio.get_sample_sum() - 0.01).abs() < 1e-9);
    }

    #[test]
    fn sessions_count_by_transport_and_family() {
      let quality = CallQuality::new();
      quality.session_joined(Transport::Quic, IpAddr::V4(Ipv4Addr::LOCALHOST));
      // A dual-stack socket's IPv4 peer.
      quality.session_joined(
        Transport::Quic,
        IpAddr::V6(Ipv4Addr::new(192, 0, 2, 1).to_ipv6_mapped()),
      );
      quality.session_joined(Transport::Fallback, IpAddr::V6(Ipv6Addr::LOCALHOST));
      let count = |transport: Transport, family: AddressFamily| {
        quality
          .sessions
          .with_label_values(&[transport.label(), family.label()])
          .get()
      };
      assert_eq!(count(Transport::Quic, AddressFamily::Ipv4), 2);
      assert_eq!(count(Transport::Quic, AddressFamily::Ipv6), 0);
      assert_eq!(count(Transport::Fallback, AddressFamily::Ipv6), 1);
    }
  }

  fn counters(sent_packets: u64, lost_packets: u64) -> PathCounters {
    PathCounters {
      sent_packets,
      lost_packets,
    }
  }
}
