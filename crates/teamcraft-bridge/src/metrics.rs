//! What the bridge exposes on `GET /metrics` (operations.md, "Metrics"; issue #213): requests by
//! route class and status, and the worker's queue. The source token is part of the webhook and log
//! paths and the bridge holds plaintext, so nothing from a path or a body ever becomes a label.

use std::net::SocketAddr;
use std::sync::Arc;

use serde::Deserialize;
use teamcraft_ops::metrics::{HttpMetrics, IntGauge, Metrics, MetricsError, loopback_addrs};

/// Default port of the metrics listener.
const DEFAULT_PORT: u16 = 9466;

/// `[metrics]`: the optional `GET /metrics` listener.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsSection {
  #[serde(rename = "enabled")]
  pub is_enabled: bool,
  pub listen_addrs: Vec<SocketAddr>,
}

/// The bridge's metrics.
pub struct BridgeMetrics {
  metrics: Metrics,
  http: Arc<HttpMetrics>,
  queue_depth: IntGauge,
}

impl Default for MetricsSection {
  fn default() -> Self {
    Self {
      is_enabled: false,
      listen_addrs: loopback_addrs(DEFAULT_PORT),
    }
  }
}

impl BridgeMetrics {
  pub fn new() -> Result<Self, MetricsError> {
    let metrics = Metrics::new("bridge", env!("CARGO_PKG_VERSION"))?;
    Ok(Self {
      http: HttpMetrics::new(&metrics, route_class)?,
      queue_depth: metrics.int_gauge(
        "teamcraft_bridge_queue_depth",
        "Submissions waiting for the worker.",
      )?,
      metrics,
    })
  }

  /// What the endpoints count their requests with.
  pub fn http(&self) -> Arc<HttpMetrics> {
    self.http.clone()
  }

  /// The output of `GET /metrics`, with `queued` submissions waiting.
  pub fn render(&self, queued: usize) -> Result<String, MetricsError> {
    self.queue_depth.set(i64::try_from(queued).unwrap_or(i64::MAX));
    self.metrics.render()
  }
}

/// The class of a request path, without its source token.
fn route_class(path: &str) -> &'static str {
  if path.starts_with("/hooks/") {
    "hooks"
  } else if path.starts_with("/ingest/") {
    "ingest"
  } else if path == "/healthz" {
    "health"
  } else {
    "other"
  }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
  use std::collections::{BTreeMap, BTreeSet};
  use std::time::Instant;

  use axum::http::StatusCode;
  use teamcraft_ops::metrics::{AllowList, STATUS_LABELS, check_output};

  use super::*;

  const TOKEN: &str = "SECRETSOURCETOKEN";

  #[test]
  fn the_output_holds_only_listed_values_and_no_source_token() {
    let metrics = BridgeMetrics::new().unwrap();
    for path in [
      format!("/hooks/{TOKEN}"),
      format!("/ingest/{TOKEN}"),
      "/healthz".to_owned(),
      format!("/{TOKEN}"),
    ] {
      metrics
        .http()
        .record_request(&path, StatusCode::OK, Instant::now());
    }
    let output = metrics.render(3).unwrap();
    assert!(!output.contains(TOKEN), "{output}");
    assert!(output.contains("teamcraft_bridge_queue_depth 3"));
    assert!(output.contains(r#"route_class="hooks""#));
    let allowed = AllowList {
      metrics: BTreeSet::from([
        "teamcraft_bridge_queue_depth",
        "teamcraft_build_info",
        "teamcraft_http_request_duration_seconds",
        "teamcraft_http_requests_total",
      ]),
      labels: BTreeMap::from([
        ("service", BTreeSet::from(["bridge"])),
        ("version", BTreeSet::from([env!("CARGO_PKG_VERSION")])),
        (
          "route_class",
          BTreeSet::from(["health", "hooks", "ingest", "other"]),
        ),
        ("status", STATUS_LABELS.into_iter().collect()),
      ]),
    };
    check_output(&output, &allowed).unwrap();
    let docs = include_str!("../../../docs/operations.md");
    for family in allowed.metrics {
      assert!(docs.contains(family), "{family} is not in docs/operations.md");
    }
  }

  #[test]
  fn metrics_are_off_and_on_loopback_by_default() {
    let section = MetricsSection::default();
    assert!(!section.is_enabled);
    assert!(section.listen_addrs.iter().all(|addr| addr.ip().is_loopback()));
  }
}
