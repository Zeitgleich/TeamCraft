//! What the SFU exposes on `GET /metrics` (operations.md, "Metrics"; issue #213): how many calls run
//! and how many devices are in them, read when a scrape comes. Forwarding media pays nothing for it.
//! Counts only: no call, user or device IDs, and the SFU can't read media anyway. How the calls go
//! (loss, round-trip time, transports; #1648) comes from [`CallQuality`], counted as it happens.

use std::net::SocketAddr;

use teamcraft_ops::metrics::{IntGauge, Metrics, MetricsError, loopback_addrs};

use crate::quality::CallQuality;
use crate::room::RoomStats;

/// Default port of the metrics listener.
const DEFAULT_PORT: u16 = 9465;

/// The SFU's metrics.
pub struct SfuMetrics {
  metrics: Metrics,
  rooms: IntGauge,
  participants: IntGauge,
  fallback_connections: IntGauge,
}

/// Why the metrics settings can't be used.
#[derive(Debug, thiserror::Error)]
pub enum MetricsConfigError {
  #[error("TEAMCRAFT_SFU_METRICS_ENABLED must be 0 or 1, not {0:?}")]
  Enabled(String),
  #[error("TEAMCRAFT_SFU_METRICS_ADDRS holds {0:?}, which is not a socket address")]
  Address(String),
}

impl SfuMetrics {
  pub fn new(quality: &CallQuality) -> Result<Self, MetricsError> {
    let metrics = Metrics::new("sfu", env!("CARGO_PKG_VERSION"))?;
    quality.register(&metrics)?;
    Ok(Self {
      rooms: metrics.int_gauge("teamcraft_sfu_rooms", "Calls running on this SFU.")?,
      participants: metrics.int_gauge(
        "teamcraft_sfu_participants",
        "Devices in those calls, reconnecting ones included.",
      )?,
      fallback_connections: metrics.int_gauge(
        "teamcraft_sfu_fallback_connections",
        "Open connections on the WebSocket fallback, for networks that block UDP.",
      )?,
      metrics,
    })
  }

  /// The output of `GET /metrics`.
  pub fn render(&self, stats: RoomStats, fallback_connections: usize) -> Result<String, MetricsError> {
    self
      .fallback_connections
      .set(i64::try_from(fallback_connections).unwrap_or(i64::MAX));
    self.rooms.set(i64::try_from(stats.rooms).unwrap_or(i64::MAX));
    self
      .participants
      .set(i64::try_from(stats.participants).unwrap_or(i64::MAX));
    self.metrics.render()
  }
}

/// The addresses to serve metrics on, from `TEAMCRAFT_SFU_METRICS_ENABLED` and
/// `TEAMCRAFT_SFU_METRICS_ADDRS`; `None` while they are off (the default).
pub fn metrics_addrs(
  maybe_enabled: Option<&str>,
  maybe_addrs: Option<&str>,
) -> Result<Option<Vec<SocketAddr>>, MetricsConfigError> {
  match maybe_enabled {
    None | Some("0") => return Ok(None),
    Some("1") => {},
    Some(other) => return Err(MetricsConfigError::Enabled(other.to_owned())),
  }
  let Some(addrs) = maybe_addrs else {
    return Ok(Some(loopback_addrs(DEFAULT_PORT)));
  };
  addrs
    .split(',')
    .map(|addr| {
      addr
        .trim()
        .parse()
        .map_err(|_| MetricsConfigError::Address(addr.to_owned()))
    })
    .collect::<Result<Vec<_>, _>>()
    .map(Some)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
  use std::collections::{BTreeMap, BTreeSet};

  use teamcraft_ops::metrics::{AllowList, check_output};

  use super::*;

  mod output {
    use super::*;

    #[test]
    fn it_holds_the_counts_and_nothing_else() {
      let quality = CallQuality::new();
      quality.keyframe_requested();
      let output = SfuMetrics::new(&quality)
        .unwrap()
        .render(
          RoomStats {
            rooms: 2,
            participants: 5,
          },
          3,
        )
        .unwrap();
      assert!(output.contains("teamcraft_sfu_rooms 2"));
      assert!(output.contains("teamcraft_sfu_fallback_connections 3"));
      assert!(output.contains("teamcraft_sfu_participants 5"));
      assert!(output.contains("teamcraft_sfu_keyframe_requests_total 1"));
      assert!(output.contains(r#"teamcraft_sfu_sessions_total{family="ipv6",transport="fallback"} 0"#));
      let allowed = AllowList {
        metrics: BTreeSet::from([
          "teamcraft_build_info",
          "teamcraft_sfu_downlink_loss_ratio",
          "teamcraft_sfu_fallback_connections",
          "teamcraft_sfu_forwarded_bytes_total",
          "teamcraft_sfu_keyframe_requests_total",
          "teamcraft_sfu_participants",
          "teamcraft_sfu_retransmitted_packets_total",
          "teamcraft_sfu_rooms",
          "teamcraft_sfu_rtt_seconds",
          "teamcraft_sfu_session_switches_total",
          "teamcraft_sfu_sessions_total",
        ]),
        labels: BTreeMap::from([
          ("transport", BTreeSet::from(["quic", "fallback"])),
          ("family", BTreeSet::from(["ipv4", "ipv6"])),
          ("service", BTreeSet::from(["sfu"])),
          ("version", BTreeSet::from([env!("CARGO_PKG_VERSION")])),
        ]),
      };
      check_output(&output, &allowed).unwrap();
      let docs = include_str!("../../../docs/operations.md");
      for family in allowed.metrics {
        assert!(docs.contains(family), "{family} is not in docs/operations.md");
      }
    }
  }

  mod settings {
    use super::*;

    #[test]
    fn metrics_are_off_unless_asked_for() {
      assert!(metrics_addrs(None, None).unwrap().is_none());
      assert!(metrics_addrs(Some("0"), Some("[::]:1")).unwrap().is_none());
      assert!(metrics_addrs(Some("yes"), None).is_err());
    }

    #[test]
    fn the_default_is_loopback() {
      let addrs = metrics_addrs(Some("1"), None).unwrap().unwrap();
      assert!(!addrs.is_empty() && addrs.iter().all(|addr| addr.ip().is_loopback()));
    }

    #[test]
    fn addresses_are_a_comma_separated_list() {
      let addrs = metrics_addrs(Some("1"), Some("[::]:9000, 127.0.0.1:9001"))
        .unwrap()
        .unwrap();
      assert_eq!(addrs.len(), 2);
      assert!(addrs[0].is_ipv6() && addrs[1].is_ipv4());
      assert!(metrics_addrs(Some("1"), Some("nonsense")).is_err());
    }
  }
}
