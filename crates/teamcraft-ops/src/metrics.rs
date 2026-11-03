//! The optional `GET /metrics` of the server binaries, in Prometheus text format (operations.md,
//! "Metrics"): a separate listener that serves nothing else, off by default, loopback by default.
//!
//! **What may be in the output:** label values come from fixed lists in the code, never from a
//! request or the database, so the number of series is bounded and no user ID, conversation ID,
//! content or token can get in. [`HttpMetrics`] takes its route classes from a function returning
//! `&'static str`, for that reason. Each binary's tests check its whole output against an
//! allow-list with [`check_output`].

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use prometheus::{Encoder, HistogramOpts, HistogramVec, Opts, Registry, TextEncoder};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::bind_dual_stack;

pub use prometheus::{IntCounterVec, IntGauge, IntGaugeVec};

/// The path of the endpoint, the only one the listener serves.
pub const METRICS_PATH: &str = "/metrics";

/// Every value of the `status` label: the class of the status code, and 429 on its own (rate limits
/// are worth telling apart from other refusals, so `4xx` doesn't include it).
pub const STATUS_LABELS: [&str; 6] = ["1xx", "2xx", "3xx", "4xx", "429", "5xx"];

/// Upper bounds of the latency histograms, in seconds: from a cached read to a request that is about
/// to time out.
const DURATION_BUCKETS_SECS: [f64; 11] = [0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];

/// The metrics of one binary: a registry of its own, not a global one, so nothing else can add to
/// the output, and tests running side by side don't meet.
#[derive(Clone)]
pub struct Metrics {
  registry: Registry,
}

/// HTTP requests by route class and status, and their latency by route class.
///
/// One request costs two label lookups (a hash of two short strings and a read lock) and two
/// atomic updates, whatever the server holds. A request the client abandons before the answer isn't
/// counted, and the latency ends at the response's head: a WebSocket upgrade counts as a `1xx`
/// that took as long as the upgrade, not as long as the socket stayed open.
pub struct HttpMetrics {
  requests: IntCounterVec,
  durations: HistogramVec,
  classify: fn(&str) -> &'static str,
}

/// What a binary's metrics output may hold.
#[derive(Debug, Clone, Default)]
pub struct AllowList {
  /// Metric family names; a histogram's `_bucket`, `_sum` and `_count` count as the family.
  pub metrics: BTreeSet<&'static str>,
  /// The label names, each with every value it may have. `le` (a number, histograms) needs no entry.
  pub labels: BTreeMap<&'static str, BTreeSet<&'static str>>,
}

/// Why the metrics of a binary can't be set up or rendered.
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
  /// Metric names are fixed in the code, so this is a bug: an invalid or a duplicate name.
  #[error("registering a metric failed: {0}")]
  Register(#[from] prometheus::Error),
  #[error("the metrics listener needs at least one address")]
  NoAddress,
  #[error("binding the metrics listener on {addr} failed: {source}")]
  Bind { addr: SocketAddr, source: io::Error },
  #[error("the rendered metrics are not UTF-8")]
  NotUtf8,
}

type Render = Arc<dyn Fn() -> Result<String, MetricsError> + Send + Sync>;

impl Metrics {
  /// A registry with `teamcraft_build_info{service, version}`.
  pub fn new(service: &'static str, version: &'static str) -> Result<Self, MetricsError> {
    let metrics = Self {
      registry: Registry::new(),
    };
    let info = metrics.int_gauge_vec(
      "teamcraft_build_info",
      "Always 1; the labels name the binary and its version.",
      &["service", "version"],
    )?;
    info.with_label_values(&[service, version]).set(1);
    Ok(metrics)
  }

  pub fn int_gauge(&self, name: &str, help: &str) -> Result<IntGauge, MetricsError> {
    let gauge = IntGauge::with_opts(Opts::new(name, help))?;
    self.registry.register(Box::new(gauge.clone()))?;
    Ok(gauge)
  }

  pub fn int_gauge_vec(&self, name: &str, help: &str, labels: &[&str]) -> Result<IntGaugeVec, MetricsError> {
    let gauge = IntGaugeVec::new(Opts::new(name, help), labels)?;
    self.registry.register(Box::new(gauge.clone()))?;
    Ok(gauge)
  }

  pub fn int_counter_vec(
    &self,
    name: &str,
    help: &str,
    labels: &[&str],
  ) -> Result<IntCounterVec, MetricsError> {
    let counter = IntCounterVec::new(Opts::new(name, help), labels)?;
    self.registry.register(Box::new(counter.clone()))?;
    Ok(counter)
  }

  /// The output of the endpoint.
  pub fn render(&self) -> Result<String, MetricsError> {
    let mut output = Vec::new();
    TextEncoder::new().encode(&self.registry.gather(), &mut output)?;
    String::from_utf8(output).map_err(|_| MetricsError::NotUtf8)
  }

  fn duration_histogram_vec(
    &self,
    name: &str,
    help: &str,
    labels: &[&str],
  ) -> Result<HistogramVec, MetricsError> {
    let histogram = HistogramVec::new(
      HistogramOpts::new(name, help).buckets(DURATION_BUCKETS_SECS.to_vec()),
      labels,
    )?;
    self.registry.register(Box::new(histogram.clone()))?;
    Ok(histogram)
  }
}

impl HttpMetrics {
  /// `classify` maps a request path to one of a fixed list of classes. It never hands back a part
  /// of the path: paths hold IDs and tokens, and anyone can make up new ones.
  pub fn new(metrics: &Metrics, classify: fn(&str) -> &'static str) -> Result<Arc<Self>, MetricsError> {
    Ok(Arc::new(Self {
      requests: metrics.int_counter_vec(
        "teamcraft_http_requests_total",
        "HTTP requests answered, by route class and status class.",
        &["route_class", "status"],
      )?,
      durations: metrics.duration_histogram_vec(
        "teamcraft_http_request_duration_seconds",
        "Time until the response's head, by route class.",
        &["route_class"],
      )?,
      classify,
    }))
  }

  /// Counts one answered request to `path`.
  pub fn record_request(&self, path: &str, status: StatusCode, started: Instant) {
    self.record((self.classify)(path), status, started);
  }

  fn record(&self, route_class: &'static str, status: StatusCode, started: Instant) {
    self
      .requests
      .with_label_values(&[route_class, status_label(status)])
      .inc();
    self
      .durations
      .with_label_values(&[route_class])
      .observe(started.elapsed().as_secs_f64());
  }
}

/// The default listen addresses for `port`: loopback on IPv4 and IPv6. Scrapers on the same host
/// reach it; anything else is the operator's decision (`[::]:port` is dual stack).
pub fn loopback_addrs(port: u16) -> Vec<SocketAddr> {
  vec![
    SocketAddr::from(([127, 0, 0, 1], port)),
    SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
  ]
}

/// `router` with its requests counted, the refusals of its other layers included.
pub fn with_http_metrics(router: Router, metrics: Arc<HttpMetrics>) -> Router {
  router.layer(middleware::from_fn_with_state(metrics, track_http))
}

async fn track_http(State(metrics): State<Arc<HttpMetrics>>, request: Request, next: Next) -> Response {
  // Only the class goes any further: the path holds IDs and tokens.
  let route_class = (metrics.classify)(request.uri().path());
  let started = Instant::now();
  let response = next.run(request).await;
  metrics.record(route_class, response.status(), started);
  response
}

fn status_label(status: StatusCode) -> &'static str {
  match status.as_u16() {
    429 => "429",
    100..=199 => "1xx",
    200..=299 => "2xx",
    300..=399 => "3xx",
    400..=499 => "4xx",
    _ => "5xx",
  }
}

/// Binds the metrics listeners. An IPv6 address that can't be bound is tolerated once another one
/// worked (hosts without IPv6, as with the bridge's endpoints); any other failure is an error, so a
/// port conflict shows at the start, not when somebody looks at an empty dashboard.
pub fn bind_metrics(addrs: &[SocketAddr]) -> Result<Vec<TcpListener>, MetricsError> {
  if addrs.is_empty() {
    return Err(MetricsError::NoAddress);
  }
  let mut listeners = Vec::with_capacity(addrs.len());
  for addr in addrs {
    match bind_dual_stack(*addr) {
      Ok(listener) => listeners.push(listener),
      Err(error) if addr.is_ipv6() && !listeners.is_empty() => {
        warn!(%addr, %error, "not serving metrics on this address");
        continue;
      },
      Err(source) => return Err(MetricsError::Bind { addr: *addr, source }),
    }
    if !addr.ip().is_loopback() {
      warn!(
        %addr,
        "metrics are reachable from the network and have no authentication: restrict access to the scraper"
      );
    }
  }
  Ok(listeners)
}

/// Serves `GET /metrics` on `listeners` until the process ends; `render` runs once per scrape. A
/// scrape never holds up the shutdown of the service.
pub fn spawn_metrics_endpoint(
  listeners: Vec<TcpListener>,
  render: impl Fn() -> Result<String, MetricsError> + Send + Sync + 'static,
) {
  let app = metrics_router(Arc::new(render));
  for listener in listeners {
    let app = app.clone();
    tokio::spawn(async move {
      if let Ok(addr) = listener.local_addr() {
        info!(%addr, "serving metrics");
      }
      // The service runs on without its metrics; the scraper notices that they are gone.
      if let Err(error) = axum::serve(listener, app).await {
        warn!(%error, "the metrics listener stopped");
      }
    });
  }
}

fn metrics_router(render: Render) -> Router {
  Router::new()
    .route(METRICS_PATH, get(metrics_handler))
    .with_state(render)
}

async fn metrics_handler(State(render): State<Render>) -> Response {
  match render() {
    Ok(body) => ([(header::CONTENT_TYPE, prometheus::TEXT_FORMAT)], body).into_response(),
    Err(error) => {
      warn!(%error, "rendering the metrics failed");
      StatusCode::INTERNAL_SERVER_ERROR.into_response()
    },
  }
}

/// Checks a rendered output against `allowed`: every sample's family, label name and label value
/// must be listed, and the value must be a number. The first thing that isn't is the error. For
/// the binaries' tests.
pub fn check_output(output: &str, allowed: &AllowList) -> Result<(), String> {
  for line in output.lines().filter(|line| !line.is_empty()) {
    let result = match line.strip_prefix('#') {
      Some(comment) => check_comment(comment, allowed),
      None => check_sample(line, allowed),
    };
    result.map_err(|error| format!("{error} in `{line}`"))?;
  }
  Ok(())
}

/// `# HELP name …` and `# TYPE name …` lines must name a listed family. The help texts are fixed
/// in the code.
fn check_comment(comment: &str, allowed: &AllowList) -> Result<(), String> {
  let mut words = comment.split_whitespace();
  match (words.next(), words.next()) {
    (Some("HELP" | "TYPE"), Some(name)) if allowed.metrics.contains(name) => Ok(()),
    _ => Err("a comment that names no listed metric".to_owned()),
  }
}

fn check_sample(line: &str, allowed: &AllowList) -> Result<(), String> {
  let (series, value) = line.rsplit_once(' ').ok_or("no value")?;
  value.parse::<f64>().map_err(|_| "a value that is no number")?;
  let (name, labels) = match series.split_once('{') {
    Some((name, labels)) => (name, labels.strip_suffix('}').ok_or("unterminated labels")?),
    None => (series, ""),
  };
  if !is_listed_family(name, allowed) {
    return Err(format!("the unlisted metric {name}"));
  }
  for pair in labels.split(',').filter(|pair| !pair.is_empty()) {
    let (label, quoted) = pair.split_once('=').ok_or("a label without a value")?;
    let value = quoted
      .strip_prefix('"')
      .and_then(|value| value.strip_suffix('"'))
      .ok_or("an unquoted label value")?;
    if label == "le" && (value == "+Inf" || value.parse::<f64>().is_ok()) {
      continue;
    }
    let values = allowed
      .labels
      .get(label)
      .ok_or_else(|| format!("the unlisted label {label}"))?;
    if !values.contains(value) {
      return Err(format!("the unlisted value {value:?} of label {label}"));
    }
  }
  Ok(())
}

fn is_listed_family(name: &str, allowed: &AllowList) -> bool {
  allowed.metrics.contains(name)
    || ["_bucket", "_sum", "_count"]
      .iter()
      .filter_map(|suffix| name.strip_suffix(suffix))
      .any(|family| allowed.metrics.contains(family))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
  use std::net::Ipv4Addr;

  use super::*;

  const VERSION: &str = "1.2.3";

  mod output {
    use super::*;

    #[test]
    fn an_empty_registry_holds_only_the_build_info() {
      let output = Metrics::new("server", VERSION).unwrap().render().unwrap();
      assert!(output.contains(r#"teamcraft_build_info{service="server",version="1.2.3"} 1"#));
      check_output(&output, &allow_list()).unwrap();
    }

    #[test]
    fn a_counter_with_an_unlisted_label_value_is_refused() {
      let metrics = Metrics::new("server", VERSION).unwrap();
      let http = HttpMetrics::new(&metrics, |_| "user-123456").unwrap();
      http.record_request("/", StatusCode::OK, Instant::now());
      let error = check_output(&metrics.render().unwrap(), &allow_list()).unwrap_err();
      assert!(error.contains("user-123456"), "{error}");
    }

    #[test]
    fn an_unlisted_metric_is_refused() {
      let error = check_output("teamcraft_secret 1\n", &allow_list()).unwrap_err();
      assert!(error.contains("teamcraft_secret"), "{error}");
      let error = check_output("# TYPE teamcraft_secret gauge\n", &allow_list()).unwrap_err();
      assert!(error.contains("teamcraft_secret"), "{error}");
    }

    #[test]
    fn malformed_lines_are_refused() {
      for line in [
        "teamcraft_build_info",
        "teamcraft_build_info nan-ish",
        "teamcraft_build_info{service=server} 1",
        r#"teamcraft_build_info{service="server" 1"#,
        r#"teamcraft_build_info{unknown="x"} 1"#,
      ] {
        assert!(check_output(line, &allow_list()).is_err(), "{line}");
      }
    }
  }

  mod http {
    use super::*;

    #[test]
    fn statuses_become_their_class_with_429_apart() {
      for (status, label) in [
        (StatusCode::SWITCHING_PROTOCOLS, "1xx"),
        (StatusCode::OK, "2xx"),
        (StatusCode::FOUND, "3xx"),
        (StatusCode::NOT_FOUND, "4xx"),
        (StatusCode::TOO_MANY_REQUESTS, "429"),
        (StatusCode::SERVICE_UNAVAILABLE, "5xx"),
      ] {
        assert_eq!(status_label(status), label);
        assert!(STATUS_LABELS.contains(&label));
      }
    }

    #[tokio::test]
    async fn a_request_is_counted_by_its_class_and_its_path_stays_out() {
      let metrics = Metrics::new("server", VERSION).unwrap();
      let http = HttpMetrics::new(&metrics, |_| "web").unwrap();
      let app = with_http_metrics(
        Router::new().route("/known/{token}", get(|| async { StatusCode::TOO_MANY_REQUESTS })),
        http,
      );
      let base = serve(app).await;
      for path in ["/known/secret-token", "/unknown/1234567890"] {
        reqwest::get(format!("{base}{path}")).await.unwrap();
      }
      let output = metrics.render().unwrap();
      assert!(output.contains(r#"teamcraft_http_requests_total{route_class="web",status="429"} 1"#));
      assert!(output.contains(r#"teamcraft_http_requests_total{route_class="web",status="4xx"} 1"#));
      assert!(output.contains(r#"teamcraft_http_request_duration_seconds_count{route_class="web"} 2"#));
      assert!(!output.contains("secret-token") && !output.contains("1234567890"));
      check_output(&output, &allow_list()).unwrap();
    }
  }

  mod endpoint {
    use super::*;

    #[tokio::test]
    async fn it_serves_the_text_format_on_its_path_and_nothing_else() {
      let base = serve(metrics_router(Arc::new(|| {
        Ok("teamcraft_build_info 1\n".to_owned())
      })))
      .await;
      let response = reqwest::get(format!("{base}{METRICS_PATH}")).await.unwrap();
      assert_eq!(response.status(), StatusCode::OK.as_u16());
      let content_type = response.headers()["content-type"].to_str().unwrap().to_owned();
      assert!(
        content_type.starts_with("text/plain; version=0.0.4"),
        "{content_type}"
      );
      assert_eq!(response.text().await.unwrap(), "teamcraft_build_info 1\n");

      let other = reqwest::get(format!("{base}/healthz")).await.unwrap();
      assert_eq!(other.status(), StatusCode::NOT_FOUND.as_u16());
      let post = reqwest::Client::new()
        .post(format!("{base}{METRICS_PATH}"))
        .send()
        .await
        .unwrap();
      assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED.as_u16());
    }

    #[tokio::test]
    async fn a_failing_render_is_a_500_without_details() {
      let base = serve(metrics_router(Arc::new(|| Err(MetricsError::NotUtf8)))).await;
      let response = reqwest::get(format!("{base}{METRICS_PATH}")).await.unwrap();
      assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR.as_u16());
      assert_eq!(response.text().await.unwrap(), "");
    }
  }

  mod binding {
    use super::*;

    #[test]
    fn the_default_addresses_are_loopback_on_both_families() {
      let addrs = loopback_addrs(9464);
      assert_eq!(addrs.len(), 2);
      assert!(
        addrs
          .iter()
          .all(|addr| addr.ip().is_loopback() && addr.port() == 9464)
      );
      assert!(addrs.iter().any(SocketAddr::is_ipv4) && addrs.iter().any(SocketAddr::is_ipv6));
    }

    #[test]
    fn no_address_is_an_error() {
      assert!(matches!(bind_metrics(&[]), Err(MetricsError::NoAddress)));
    }

    #[tokio::test]
    async fn a_taken_ipv4_port_is_an_error_at_the_start() {
      let taken = bind_dual_stack((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
      let addr = taken.local_addr().unwrap();
      assert!(matches!(bind_metrics(&[addr]), Err(MetricsError::Bind { .. })));
    }

    #[tokio::test]
    async fn an_ipv6_address_that_fails_after_another_worked_is_tolerated() {
      // Hosts without IPv6 cannot bind `[::1]`; nothing to test there.
      let Ok(taken) = bind_dual_stack((Ipv6Addr::LOCALHOST, 0).into()) else {
        return;
      };
      let taken_addr = taken.local_addr().unwrap();
      let free = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
      assert_eq!(bind_metrics(&[free, taken_addr]).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_unspecified_ipv6_address_serves_both_families() {
      let Ok(listeners) = bind_metrics(&[SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0))]) else {
        return;
      };
      let port = listeners[0].local_addr().unwrap().port();
      spawn_metrics_endpoint(listeners, || Ok("teamcraft_build_info 1\n".to_owned()));
      for host in ["[::1]", "127.0.0.1"] {
        let response = reqwest::get(format!("http://{host}:{port}{METRICS_PATH}"))
          .await
          .unwrap();
        assert_eq!(response.status(), StatusCode::OK.as_u16(), "{host}");
      }
    }
  }

  fn allow_list() -> AllowList {
    AllowList {
      metrics: [
        "teamcraft_build_info",
        "teamcraft_http_requests_total",
        "teamcraft_http_request_duration_seconds",
      ]
      .into(),
      labels: [
        ("service", ["server"].into()),
        ("version", [VERSION].into()),
        ("route_class", ["web"].into()),
        ("status", STATUS_LABELS.into()),
      ]
      .into(),
    }
  }

  /// Serves `app` on a free loopback port; its base URL.
  async fn serve(app: Router) -> String {
    let listener = bind_dual_stack((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{addr}")
  }
}
