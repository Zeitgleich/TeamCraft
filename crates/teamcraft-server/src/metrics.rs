//! What the server exposes on `GET /metrics` (operations.md, "Metrics"; issue #213).
//!
//! The hot paths pay for HTTP counters only, and only with the listener on. Everything else is
//! read when a scrape comes, from counters and states the server keeps anyway. What needs a query
//! (database size, queued wake-ups) comes from a periodic check, never from a request or a scrape.
//! Label values come from the fixed lists below; see [`teamcraft_ops::metrics`].

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::Deserialize;
use teamcraft_db::{Database, DbError, InstanceStats, PoolStats};
use teamcraft_ops::metrics::{
  HttpMetrics, IntCounterVec, IntGauge, IntGaugeVec, Metrics, MetricsError, loopback_addrs,
};
use teamcraft_proto::{DependencyHealth, DependencyName, DependencyState, PushProvider};
use time::OffsetDateTime;
use tracing::warn;

use crate::gateway::HubStats;
use crate::push::ProviderHealth;
use crate::state::AppState;

/// Default port of the metrics listener.
const DEFAULT_PORT: u16 = 9464;
/// Often enough for a dashboard; `pg_database_size` walks the database's files, which a request
/// or a scrape must not wait for.
const DEFAULT_DATABASE_CHECK_INTERVAL_SECS: u64 = 60;

/// Each provider with its `provider` label. A list of its own: a device's registration can name a
/// provider this server doesn't know, and that must not become a label.
const PUSH_PROVIDERS: [(PushProvider, &str); 4] = [
  (PushProvider::Apns, "apns"),
  (PushProvider::Fcm, "fcm"),
  (PushProvider::UnifiedPush, "unifiedpush"),
  (PushProvider::WebPush, "webpush"),
];

/// Every value of the `state` label of `teamcraft_dependency_state`.
const DEPENDENCY_STATES: [&str; 3] = ["ok", "down", "recovering"];

/// `[metrics]`: the optional `GET /metrics` listener. Bootstrap configuration: it has to be known
/// before the database is read, and it decides what is reachable on the host.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MetricsConfig {
  #[serde(rename = "enabled")]
  pub is_enabled: bool,
  /// Separate from `server.listen_addr`, so the reverse proxy doesn't publish it.
  pub listen_addrs: Vec<SocketAddr>,
  /// How often the database size and the queued wake-ups are read.
  pub database_check_interval_secs: u64,
}

/// The background jobs whose last success is exported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundJob {
  DatabaseCheck,
  EmailDigest,
  GuestExpiry,
  HolidayResync,
  KeyPackagePruning,
  OidcSync,
  PartitionMaintenance,
  Retention,
  ScheduledMessages,
  UrgentScheduler,
  MeetingNotices,
}

/// What DPoP made of a request with an identity provider's token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpopOutcome {
  /// Bound, with a valid proof.
  Bound,
  /// Not bound, and accepted: a client the policy `required` would lock out.
  Unbound,
  Refused,
}

/// The server's metrics.
pub struct ServerMetrics {
  metrics: Metrics,
  http: Arc<HttpMetrics>,
  gateway_sessions: IntGauge,
  gateway_connections: IntGauge,
  gateway_buffer_bytes: IntGauge,
  db_pool_connections: IntGaugeVec,
  db_pool_max_connections: IntGauge,
  db_size_bytes: IntGauge,
  push_retries_pending: IntGauge,
  push_sends: IntCounterVec,
  push_failures: IntCounterVec,
  push_endpoint_failures: IntCounterVec,
  push_provider_failing: IntGaugeVec,
  dependency_state: IntGaugeVec,
  job_last_success: IntGaugeVec,
  dpop_requests: IntCounterVec,
  /// One scrape at a time: the push counters are brought up to date by their difference.
  scrape: Mutex<()>,
}

/// What a scrape reads from the running server.
struct Snapshot {
  hub: HubStats,
  pool: PoolStats,
  providers: Vec<(&'static str, ProviderHealth)>,
  dependencies: Vec<DependencyHealth>,
}

impl Default for MetricsConfig {
  fn default() -> Self {
    Self {
      is_enabled: false,
      listen_addrs: loopback_addrs(DEFAULT_PORT),
      database_check_interval_secs: DEFAULT_DATABASE_CHECK_INTERVAL_SECS,
    }
  }
}

impl MetricsConfig {
  pub fn database_check_interval(&self) -> Duration {
    Duration::from_secs(self.database_check_interval_secs.max(1))
  }
}

impl DpopOutcome {
  const ALL: [Self; 3] = [Self::Bound, Self::Unbound, Self::Refused];

  fn label(self) -> &'static str {
    match self {
      Self::Bound => "bound",
      Self::Unbound => "unbound",
      Self::Refused => "refused",
    }
  }
}

impl BackgroundJob {
  const ALL: [Self; 11] = [
    Self::DatabaseCheck,
    Self::EmailDigest,
    Self::GuestExpiry,
    Self::HolidayResync,
    Self::KeyPackagePruning,
    Self::OidcSync,
    Self::PartitionMaintenance,
    Self::Retention,
    Self::ScheduledMessages,
    Self::UrgentScheduler,
    Self::MeetingNotices,
  ];

  fn label(self) -> &'static str {
    match self {
      Self::DatabaseCheck => "database_check",
      Self::EmailDigest => "email_digest",
      Self::GuestExpiry => "guest_expiry",
      Self::HolidayResync => "holiday_resync",
      Self::KeyPackagePruning => "key_package_pruning",
      Self::OidcSync => "oidc_sync",
      Self::PartitionMaintenance => "partition_maintenance",
      Self::Retention => "retention",
      Self::ScheduledMessages => "scheduled_messages",
      Self::UrgentScheduler => "urgent_scheduler",
      Self::MeetingNotices => "meeting_notices",
    }
  }
}

impl ServerMetrics {
  pub fn new() -> Result<Self, MetricsError> {
    let metrics = Metrics::new("server", env!("CARGO_PKG_VERSION"))?;
    let provider = ["provider"];
    let this = Self {
      http: HttpMetrics::new(&metrics, route_class)?,
      gateway_sessions: metrics.int_gauge(
        "teamcraft_gateway_sessions",
        "Gateway sessions, also those waiting to be resumed.",
      )?,
      gateway_connections: metrics.int_gauge(
        "teamcraft_gateway_connections",
        "Gateway sessions with a live connection.",
      )?,
      gateway_buffer_bytes: metrics.int_gauge(
        "teamcraft_gateway_resume_buffer_bytes",
        "Bytes of events buffered for resuming gateway sessions.",
      )?,
      db_pool_connections: metrics.int_gauge_vec(
        "teamcraft_db_pool_connections",
        "Connections of the database pool, by state.",
        &["state"],
      )?,
      db_pool_max_connections: metrics.int_gauge(
        "teamcraft_db_pool_max_connections",
        "The most connections the database pool may open.",
      )?,
      db_size_bytes: metrics.int_gauge(
        "teamcraft_db_size_bytes",
        "Size of the database on disk, from the last periodic check.",
      )?,
      push_retries_pending: metrics.int_gauge(
        "teamcraft_push_retries_pending",
        "Wake-ups queued for another attempt, from the last periodic check.",
      )?,
      push_sends: metrics.int_counter_vec(
        "teamcraft_push_sends_total",
        "Wake-ups the push service took, or answered with a dead registration.",
        &provider,
      )?,
      push_failures: metrics.int_counter_vec(
        "teamcraft_push_failures_total",
        "Sends that failed at the push service.",
        &provider,
      )?,
      push_endpoint_failures: metrics.int_counter_vec(
        "teamcraft_push_endpoint_failures_total",
        "Sends that failed at an endpoint a user registered (Web Push, UnifiedPush).",
        &provider,
      )?,
      push_provider_failing: metrics.int_gauge_vec(
        "teamcraft_push_provider_failing",
        "1 while the last attempt that says anything about the push service failed.",
        &provider,
      )?,
      dependency_state: metrics.int_gauge_vec(
        "teamcraft_dependency_state",
        "1 for the state the dependency's circuit breaker is in, else 0.",
        &["dependency", "state"],
      )?,
      job_last_success: metrics.int_gauge_vec(
        "teamcraft_job_last_success_timestamp_seconds",
        "When the background job last succeeded (Unix time); 0: not since the start.",
        &["job"],
      )?,
      dpop_requests: metrics.int_counter_vec(
        "teamcraft_dpop_requests_total",
        "Requests with an identity provider's token, by what DPoP made of them.",
        &["outcome"],
      )?,
      scrape: Mutex::new(()),
      metrics,
    };
    for job in BackgroundJob::ALL {
      this.job_last_success.with_label_values(&[job.label()]).set(0);
    }
    for outcome in DpopOutcome::ALL {
      this.dpop_requests.with_label_values(&[outcome.label()]).inc_by(0);
    }
    Ok(this)
  }

  /// Counts a request with an identity provider's token. What tells an admin that nobody is left
  /// who would be locked out by the policy `required` (operations.md, "Binding tokens to devices").
  pub fn note_dpop(&self, outcome: DpopOutcome) {
    self.dpop_requests.with_label_values(&[outcome.label()]).inc();
  }

  /// What the HTTP router counts its requests with, with the listener on.
  pub fn http(&self) -> Arc<HttpMetrics> {
    self.http.clone()
  }

  /// When the job last succeeded since the start, if it has.
  pub fn job_last_success(&self, job: BackgroundJob) -> Option<OffsetDateTime> {
    match self.job_last_success.with_label_values(&[job.label()]).get() {
      0 => None,
      seconds => OffsetDateTime::from_unix_timestamp(seconds).ok(),
    }
  }

  /// A background job finished without an error at `now`.
  pub fn job_succeeded(&self, job: BackgroundJob, now: OffsetDateTime) {
    self
      .job_last_success
      .with_label_values(&[job.label()])
      .set(now.unix_timestamp());
  }

  /// Reads the numbers that need a query, for at most `timeout`. A failure keeps the last values:
  /// the job's timestamp shows how old they are.
  pub async fn check_database(&self, database: &Database, timeout: Duration, now: OffsetDateTime) {
    match tokio::time::timeout(timeout, database.instance_stats()).await {
      Ok(result) => self.on_database_check(result, now),
      Err(_) => warn!(
        timeout_ms = timeout.as_millis(),
        "reading the database size for the metrics timed out"
      ),
    }
  }

  /// The output of `GET /metrics`. Takes the hub's lock for one pass over the sessions and costs
  /// nothing that depends on the size of the history. Called once per scrape, never per request.
  pub fn render(&self, state: &AppState) -> Result<String, MetricsError> {
    let _scrape = self.scrape.lock().unwrap_or_else(PoisonError::into_inner);
    self.record(&snapshot(state));
    self.metrics.render()
  }

  fn on_database_check(&self, result: Result<InstanceStats, DbError>, now: OffsetDateTime) {
    match result {
      Ok(stats) => {
        self.db_size_bytes.set(stats.database_size_bytes);
        self.push_retries_pending.set(stats.push_retries_pending);
        self.job_succeeded(BackgroundJob::DatabaseCheck, now);
      },
      Err(error) => warn!(%error, "reading the database size for the metrics failed"),
    }
  }

  fn record(&self, snapshot: &Snapshot) {
    self.gateway_sessions.set(gauge_value(snapshot.hub.sessions));
    self
      .gateway_connections
      .set(gauge_value(snapshot.hub.connections));
    self
      .gateway_buffer_bytes
      .set(gauge_value(snapshot.hub.buffer_bytes));
    self.record_pool(&snapshot.pool);
    for (label, health) in &snapshot.providers {
      self.record_provider(label, health);
    }
    for dependency in &snapshot.dependencies {
      self.record_dependency(dependency);
    }
  }

  fn record_pool(&self, pool: &PoolStats) {
    let idle = i64::from(pool.idle);
    self.db_pool_connections.with_label_values(&["idle"]).set(idle);
    self
      .db_pool_connections
      .with_label_values(&["in_use"])
      .set(i64::from(pool.size).saturating_sub(idle));
    self.db_pool_max_connections.set(i64::from(pool.max));
  }

  /// The counts live in the dispatcher (they also feed the admin console); the counters follow.
  fn record_provider(&self, label: &str, health: &ProviderHealth) {
    for (counter, total) in [
      (&self.push_sends, health.success_count),
      (&self.push_failures, health.failure_count),
      (&self.push_endpoint_failures, health.endpoint_failure_count),
    ] {
      let counter = counter.with_label_values(&[label]);
      counter.inc_by(total.saturating_sub(counter.get()));
    }
    self
      .push_provider_failing
      .with_label_values(&[label])
      .set(i64::from(health.is_failing()));
  }

  fn record_dependency(&self, dependency: &DependencyHealth) {
    // Our own breakers: an unknown name can't come up, but no label comes from anywhere else.
    let Some(name) = dependency_label(&dependency.dependency) else {
      return;
    };
    let current = dependency_state_label(&dependency.state);
    for state in DEPENDENCY_STATES {
      self
        .dependency_state
        .with_label_values(&[name, state])
        .set(i64::from(Some(state) == current));
    }
  }
}

fn snapshot(state: &AppState) -> Snapshot {
  Snapshot {
    hub: state.hub.stats(),
    pool: state.database.pool_stats(),
    providers: PUSH_PROVIDERS
      .iter()
      .map(|(provider, label)| (*label, state.push.health(provider)))
      .collect(),
    dependencies: state.dependencies.health(),
  }
}

fn gauge_value(count: usize) -> i64 {
  i64::try_from(count).unwrap_or(i64::MAX)
}

fn dependency_label(name: &DependencyName) -> Option<&'static str> {
  match name {
    DependencyName::Database => Some("database"),
    DependencyName::ObjectStorage => Some("object_storage"),
    DependencyName::IdentityProvider => Some("identity_provider"),
    DependencyName::PushGateway => Some("push_gateway"),
    DependencyName::Sfu => Some("sfu"),
    DependencyName::Unknown(_) => None,
  }
}

fn dependency_state_label(state: &DependencyState) -> Option<&'static str> {
  match state {
    DependencyState::Ok => Some("ok"),
    DependencyState::Down => Some("down"),
    DependencyState::Recovering => Some("recovering"),
    DependencyState::Unknown(_) => None,
  }
}

/// The class of a request path, from a fixed list: by its first segments, so the work doesn't depend
/// on the route table, and an ID or a token in the path goes no further than this function.
pub fn route_class(path: &str) -> &'static str {
  if path == "/healthz" || path == "/readyz" {
    return "health";
  }
  if path.starts_with("/api/internal/") {
    return "internal";
  }
  let Some(api_path) = path.strip_prefix("/api/v1/") else {
    return "web";
  };
  let mut segments = api_path.splitn(3, '/');
  let area = segments.next().unwrap_or_default();
  let _id = segments.next();
  let maybe_tail = segments.next();
  match area {
    "conversations" => conversation_class(maybe_tail),
    "dms" | "group-dms" | "threads" | "unread" => "conversations",
    "admin" | "admin-join-requests" | "announcement" | "brands" | "organizations" | "workspace" => "admin",
    "attachments" | "org-backgrounds" | "shares" => "attachments",
    "auth" => "auth",
    "calls" | "notetaker" => "calls",
    "device-links" | "devices" | "push" => "devices",
    "org-recovery" | "role-evidence" => "e2ee",
    "gateway" | "rendezvous" => "gateway",
    "meetings" => "meetings",
    "brand" | "client-compatibility" | "crash-reports" | "openapi.json" | "server-info" => "public",
    "guests" | "spaces" => "spaces",
    "sync" => "sync",
    "directory" | "me" | "presence" | "user-groups" | "users" => "users",
    _ => "other",
  }
}

/// `/api/v1/conversations/{id}/…`: messages are what the instance is for, so they get a class.
fn conversation_class(maybe_tail: Option<&str>) -> &'static str {
  let next = maybe_tail.and_then(|tail| tail.split('/').next());
  match next {
    Some("messages" | "threads") => "messages",
    Some("mls") => "e2ee",
    Some("call") => "calls",
    _ => "conversations",
  }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
  use std::collections::{BTreeMap, BTreeSet};
  use std::time::Instant;

  use axum::http::StatusCode;
  use teamcraft_ops::metrics::{AllowList, STATUS_LABELS, check_output};
  use teamcraft_proto::UnixMillis;

  use super::*;

  const SECRET_PATHS: [&str; 7] = [
    "/api/v1/conversations/7300000000000000001/messages",
    "/api/v1/users/7300000000000000002",
    "/api/v1/meetings/link/SECRETMEETINGTOKEN/knock",
    "/meeting/SECRETMEETINGTOKEN",
    "/f/SECRETSHARELINK",
    "/api/v1/SECRETAREA/x",
    "/SECRETPATH",
  ];

  mod allow_list {
    use super::*;

    #[test]
    fn the_whole_output_holds_only_listed_names_and_values() {
      let metrics = exercised_metrics();
      let output = metrics.metrics.render().unwrap();
      check_output(&output, &allowed()).unwrap();
    }

    #[test]
    fn every_family_is_in_the_output_and_in_the_docs() {
      let metrics = exercised_metrics();
      let output = metrics.metrics.render().unwrap();
      let docs = include_str!("../../../docs/operations.md");
      for family in allowed().metrics {
        assert!(output.contains(family), "{family} is not exported");
        assert!(docs.contains(family), "{family} is not in docs/operations.md");
      }
    }

    #[test]
    fn no_identifier_or_token_of_a_request_or_a_device_gets_in() {
      let metrics = exercised_metrics();
      let output = metrics.metrics.render().unwrap();
      assert!(!output.contains("SECRET"), "{output}");
      assert!(!output.contains("73000000000000000"), "{output}");
      assert!(!output.contains("made-up"), "{output}");
    }
  }

  mod route_classes {
    use super::*;

    #[test]
    fn every_class_is_listed() {
      let classes = &allowed().labels["route_class"];
      for path in SECRET_PATHS.into_iter().map(str::to_owned).chain(sample_paths()) {
        let class = route_class(&path);
        assert!(classes.contains(class), "{path}: {class}");
      }
    }

    #[test]
    fn every_documented_route_has_a_class_of_its_own() {
      for path in crate::openapi::openapi().paths.paths.keys() {
        assert_ne!(route_class(path), "other", "{path}");
      }
    }

    #[test]
    fn the_routes_that_matter_are_told_apart() {
      for (path, class) in [
        ("/healthz", "health"),
        ("/readyz", "health"),
        ("/api/v1/conversations", "conversations"),
        ("/api/v1/conversations/1", "conversations"),
        ("/api/v1/conversations/1/messages", "messages"),
        ("/api/v1/conversations/1/messages/2/reactions", "messages"),
        ("/api/v1/conversations/1/mls/commit", "e2ee"),
        ("/api/v1/conversations/1/call", "calls"),
        ("/api/v1/gateway", "gateway"),
        ("/api/v1/sync", "sync"),
        ("/api/v1/admin/users", "admin"),
        ("/api/v1/guests/expiring", "spaces"),
        ("/api/internal/v1/calls/roster", "internal"),
        ("/index.html", "web"),
        ("/api/v1/does-not-exist", "other"),
        ("/api/v1/", "other"),
      ] {
        assert_eq!(route_class(path), class, "{path}");
      }
    }
  }

  mod scrapes {
    use super::*;

    #[test]
    fn push_counters_follow_the_dispatchers_counts_without_counting_twice() {
      let metrics = ServerMetrics::new().unwrap();
      let health = ProviderHealth {
        success_count: 5,
        failure_count: 2,
        ..ProviderHealth::default()
      };
      metrics.record_provider("fcm", &health);
      metrics.record_provider("fcm", &health);
      let output = metrics.metrics.render().unwrap();
      assert!(output.contains(r#"teamcraft_push_sends_total{provider="fcm"} 5"#));
      assert!(output.contains(r#"teamcraft_push_failures_total{provider="fcm"} 2"#));
    }

    #[test]
    fn a_failing_provider_shows_until_a_success_is_newer() {
      let metrics = ServerMetrics::new().unwrap();
      let mut health = ProviderHealth {
        maybe_last_failure_at: Some(UnixMillis(2)),
        ..ProviderHealth::default()
      };
      metrics.record_provider("apns", &health);
      assert!(
        metrics
          .metrics
          .render()
          .unwrap()
          .contains(r#"teamcraft_push_provider_failing{provider="apns"} 1"#)
      );
      health.maybe_last_success_at = Some(UnixMillis(3));
      metrics.record_provider("apns", &health);
      assert!(
        metrics
          .metrics
          .render()
          .unwrap()
          .contains(r#"teamcraft_push_provider_failing{provider="apns"} 0"#)
      );
    }

    #[test]
    fn a_breaker_shows_one_state_at_a_time() {
      let metrics = ServerMetrics::new().unwrap();
      metrics.record_dependency(&DependencyHealth {
        dependency: DependencyName::ObjectStorage,
        state: DependencyState::Down,
      });
      let output = metrics.metrics.render().unwrap();
      assert!(output.contains(r#"teamcraft_dependency_state{dependency="object_storage",state="down"} 1"#));
      assert!(output.contains(r#"teamcraft_dependency_state{dependency="object_storage",state="ok"} 0"#));
    }

    #[test]
    fn saturation_is_the_connections_in_use_over_the_maximum() {
      let metrics = ServerMetrics::new().unwrap();
      metrics.record_pool(&PoolStats {
        size: 7,
        idle: 2,
        max: 10,
      });
      let output = metrics.metrics.render().unwrap();
      assert!(output.contains(r#"teamcraft_db_pool_connections{state="in_use"} 5"#));
      assert!(output.contains(r#"teamcraft_db_pool_connections{state="idle"} 2"#));
      assert!(output.contains("teamcraft_db_pool_max_connections 10"));
    }
  }

  mod database_check {
    use super::*;

    #[test]
    fn a_success_records_the_numbers_and_the_time() {
      let metrics = ServerMetrics::new().unwrap();
      metrics.on_database_check(Ok(stats(1000, 3)), at(1_700_000_000));
      let output = metrics.metrics.render().unwrap();
      assert!(output.contains("teamcraft_db_size_bytes 1000"));
      assert!(output.contains("teamcraft_push_retries_pending 3"));
      assert!(job_line("database_check", 1_700_000_000, &output));
    }

    #[test]
    fn a_failure_keeps_the_last_numbers_and_the_time_goes_stale() {
      let metrics = ServerMetrics::new().unwrap();
      metrics.on_database_check(Ok(stats(1000, 3)), at(1_700_000_000));
      metrics.on_database_check(
        Err(DbError::InvalidStoredValue("made up".to_owned())),
        at(1_700_000_060),
      );
      let output = metrics.metrics.render().unwrap();
      assert!(output.contains("teamcraft_db_size_bytes 1000"));
      assert!(job_line("database_check", 1_700_000_000, &output));
    }

    #[test]
    fn jobs_start_at_zero() {
      let output = ServerMetrics::new().unwrap().metrics.render().unwrap();
      for job in BackgroundJob::ALL {
        assert!(job_line(job.label(), 0, &output), "{job:?}");
      }
    }
  }

  mod config {
    use super::*;

    #[test]
    fn it_is_off_and_on_loopback_by_default() {
      let config = MetricsConfig::default();
      assert!(!config.is_enabled);
      assert!(!config.listen_addrs.is_empty());
      assert!(config.listen_addrs.iter().all(|addr| addr.ip().is_loopback()));
    }

    #[test]
    fn a_zero_interval_is_not_a_busy_loop() {
      let config = MetricsConfig {
        database_check_interval_secs: 0,
        ..MetricsConfig::default()
      };
      assert_eq!(config.database_check_interval(), Duration::from_secs(1));
    }
  }

  /// Metrics with every family filled, from worst-case input.
  fn exercised_metrics() -> ServerMetrics {
    let metrics = ServerMetrics::new().unwrap();
    metrics.record(&Snapshot {
      hub: HubStats {
        sessions: 4,
        connections: 3,
        buffer_bytes: 2048,
      },
      pool: PoolStats {
        size: 2,
        idle: 1,
        max: 10,
      },
      providers: PUSH_PROVIDERS
        .iter()
        .map(|(_, label)| {
          (
            *label,
            ProviderHealth {
              success_count: 1,
              failure_count: 1,
              endpoint_failure_count: 1,
              // What the push service said; it must stay out.
              maybe_last_error: Some("made-up error SECRET".to_owned()),
              ..ProviderHealth::default()
            },
          )
        })
        .collect(),
      dependencies: vec![
        DependencyHealth {
          dependency: DependencyName::Database,
          state: DependencyState::Ok,
        },
        DependencyHealth {
          dependency: DependencyName::Unknown("made-up-dependency".to_owned()),
          state: DependencyState::Down,
        },
        DependencyHealth {
          dependency: DependencyName::Sfu,
          state: DependencyState::Unknown("made-up-state".to_owned()),
        },
      ],
    });
    metrics.on_database_check(Ok(stats(1, 1)), at(1_700_000_000));
    for path in SECRET_PATHS.into_iter().map(str::to_owned).chain(sample_paths()) {
      metrics.http.record_request(&path, StatusCode::OK, Instant::now());
    }
    metrics
  }

  fn allowed() -> AllowList {
    let providers = ["apns", "fcm", "unifiedpush", "webpush"];
    AllowList {
      metrics: BTreeSet::from([
        "teamcraft_build_info",
        "teamcraft_db_pool_connections",
        "teamcraft_db_pool_max_connections",
        "teamcraft_db_size_bytes",
        "teamcraft_dependency_state",
        "teamcraft_dpop_requests_total",
        "teamcraft_gateway_connections",
        "teamcraft_gateway_resume_buffer_bytes",
        "teamcraft_gateway_sessions",
        "teamcraft_http_request_duration_seconds",
        "teamcraft_http_requests_total",
        "teamcraft_job_last_success_timestamp_seconds",
        "teamcraft_push_endpoint_failures_total",
        "teamcraft_push_failures_total",
        "teamcraft_push_provider_failing",
        "teamcraft_push_retries_pending",
        "teamcraft_push_sends_total",
      ]),
      labels: BTreeMap::from([
        ("service", BTreeSet::from(["server"])),
        ("version", BTreeSet::from([env!("CARGO_PKG_VERSION")])),
        (
          "route_class",
          BTreeSet::from([
            "admin",
            "attachments",
            "auth",
            "calls",
            "conversations",
            "devices",
            "e2ee",
            "gateway",
            "health",
            "internal",
            "meetings",
            "messages",
            "other",
            "public",
            "spaces",
            "sync",
            "users",
            "web",
          ]),
        ),
        ("status", STATUS_LABELS.into_iter().collect()),
        ("provider", providers.into_iter().collect()),
        (
          "dependency",
          BTreeSet::from([
            "database",
            "identity_provider",
            "object_storage",
            "push_gateway",
            "sfu",
          ]),
        ),
        (
          "state",
          BTreeSet::from(["down", "idle", "in_use", "ok", "recovering"]),
        ),
        ("outcome", BTreeSet::from(["bound", "refused", "unbound"])),
        (
          "job",
          BTreeSet::from([
            "database_check",
            "email_digest",
            "guest_expiry",
            "holiday_resync",
            "key_package_pruning",
            "meeting_notices",
            "oidc_sync",
            "partition_maintenance",
            "retention",
            "scheduled_messages",
            "urgent_scheduler",
          ]),
        ),
      ]),
    }
  }

  fn sample_paths() -> Vec<String> {
    crate::openapi::openapi().paths.paths.keys().cloned().collect()
  }

  fn stats(database_size_bytes: i64, push_retries_pending: i64) -> InstanceStats {
    InstanceStats {
      database_size_bytes,
      push_retries_pending,
    }
  }

  fn at(unix_seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(unix_seconds).unwrap()
  }

  fn job_line(job: &str, value: i64, output: &str) -> bool {
    output.contains(&format!(
      r#"teamcraft_job_last_success_timestamp_seconds{{job="{job}"}} {value}"#
    ))
  }
}
