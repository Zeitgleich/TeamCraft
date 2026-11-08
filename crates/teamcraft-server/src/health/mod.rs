//! The server's own view of its health, and the alerts for admins (issue #213, docs/operations.md
//! "Health and alerts").
//!
//! A background task evaluates a fixed list of checks every `[health] check_interval_secs` from
//! facts the server keeps anyway: circuit breakers, push results, when jobs last ran, and three
//! small files the deployment writes (backup, certificate, disk). It costs no database query and
//! no call to a backend. A check that fails for the hold time becomes an alert (see [`tracker`]),
//! and the admins are told through the gateway; the console reads `GET /api/v1/admin/health` when
//! it is told, or when the health page is open.
//!
//! Nothing here carries content, names or addresses: a check is one of a fixed list, with a state,
//! times and one number.

mod checks;
mod heartbeat;
mod tracker;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration as StdDuration;

use serde::Deserialize;
use teamcraft_proto::{AdminHealthResponse, GatewayEvent, HealthAlertSettings, PushProvider, UserId};
use time::{Duration, OffsetDateTime};
use tracing::{info, warn};

use crate::metrics::BackgroundJob;
use crate::state::AppState;

pub use checks::{ALL_CHECKS, JobSchedule, JobSchedules};
use checks::{Facts, MediaServerFacts, PushFacts, observe};
use heartbeat::Heartbeats;
use tracker::{AlertChangeKind, AlertPolicy, AlertTracker};

/// Shortest interval between two evaluations: they are cheap, but never a busy loop.
const MIN_CHECK_INTERVAL_SECS: u64 = 5;
/// An evaluation that comes later than this many intervals after the last one lost the thread of
/// what it was counting (see [`tracker`]).
const GAP_INTERVALS: i32 = 4;
/// The admins are read again after this many evaluations, so that an alert about the database can
/// still reach them when the database is the problem.
const ADMIN_REFRESH_EVERY: u64 = 60;

/// The providers the push check looks at.
const PUSH_PROVIDERS: [PushProvider; 4] = [
  PushProvider::WebPush,
  PushProvider::Fcm,
  PushProvider::UnifiedPush,
  PushProvider::Apns,
];

/// `[health]`: where the deployment's reports are, and how often the checks run. Bootstrap
/// configuration: it names a path on the host. When alerts are raised and what counts as too old
/// or too full are settings in the admin console.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct HealthConfig {
  /// The directory the deployment writes its reports to. Without it backup, certificate and disk
  /// show as not set up.
  #[serde(rename = "heartbeat_dir")]
  pub maybe_heartbeat_dir: Option<PathBuf>,
  /// How often the checks are evaluated.
  pub check_interval_secs: u64,
  /// After a start, checks that need history (a job that ran, the media server's first report)
  /// give no verdict for this long, so a restart raises nothing.
  pub startup_grace_secs: u64,
}

/// The checks, their state and the alerts raised since the start.
pub struct HealthMonitor {
  config: HealthConfig,
  schedules: JobSchedules,
  started_at: OffsetDateTime,
  inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
  tracker: AlertTracker,
  /// Who is told. Read from the database; kept for when the database is what failed.
  admins: Vec<UserId>,
  evaluations: u64,
}

impl Default for HealthConfig {
  fn default() -> Self {
    Self {
      maybe_heartbeat_dir: None,
      check_interval_secs: 30,
      startup_grace_secs: 300,
    }
  }
}

impl HealthConfig {
  pub fn check_interval(&self) -> StdDuration {
    StdDuration::from_secs(self.check_interval_secs.max(MIN_CHECK_INTERVAL_SECS))
  }
}

impl HealthMonitor {
  pub fn new(config: HealthConfig, schedules: JobSchedules, started_at: OffsetDateTime) -> Self {
    Self {
      config,
      schedules,
      started_at,
      inner: Mutex::new(Inner::default()),
    }
  }

  /// The last evaluation, as the console reads it.
  pub fn response(&self, settings: &HealthAlertSettings) -> AdminHealthResponse {
    let inner = self.lock();
    AdminHealthResponse {
      checks: inner.tracker.checks(&ALL_CHECKS),
      alerts: inner.tracker.alerts(),
      is_alerting_enabled: settings.is_enabled,
      hold_secs: settings.hold_secs,
      started_at: tracker::millis(self.started_at),
      maybe_checked_at: inner.tracker.maybe_last_evaluation().map(tracker::millis),
    }
  }

  /// One evaluation at `now`. Tells the admins if an alert was raised or resolved.
  pub async fn evaluate(&self, state: &AppState, now: OffsetDateTime) {
    let settings = state.settings.get().health_alerts.clone();
    let heartbeats = Heartbeats::read(self.config.maybe_heartbeat_dir.as_deref()).await;
    let observations = observe(&self.facts(state, now, settings.clone(), heartbeats));
    let (changes, is_admin_refresh_due) = {
      let mut inner = self.lock();
      let changes = inner
        .tracker
        .evaluate(now, &self.policy(&settings), &observations);
      let is_due = inner.evaluations.is_multiple_of(ADMIN_REFRESH_EVERY);
      inner.evaluations += 1;
      (changes, is_due)
    };
    for change in &changes {
      match change.kind {
        AlertChangeKind::Raised => warn!(check = %change.check, "health alert raised"),
        AlertChangeKind::Resolved => info!(check = %change.check, "health alert resolved"),
      }
    }
    if !changes.is_empty() {
      self.tell_admins(state).await;
    } else if is_admin_refresh_due {
      self.read_admins(state).await;
    }
  }

  fn facts(
    &self,
    state: &AppState,
    now: OffsetDateTime,
    settings: HealthAlertSettings,
    heartbeats: Heartbeats,
  ) -> Facts {
    let push: Vec<PushFacts> = PUSH_PROVIDERS
      .iter()
      .map(|provider| PushFacts {
        is_configured: state.push.is_configured(provider),
        is_failing: state.push.health(provider).is_failing(),
      })
      .collect();
    let media = match &state.maybe_calls {
      Some(calls) if calls.is_reporting_enabled() => MediaServerFacts::Reporting {
        maybe_last_contact: calls.last_sfu_contact(),
      },
      _ => MediaServerFacts::NotMonitored,
    };
    Facts {
      now,
      started_at: self.started_at,
      startup_grace: Duration::seconds(i64::try_from(self.config.startup_grace_secs).unwrap_or(i64::MAX)),
      settings,
      dependencies: state.dependencies.health(),
      has_object_storage: state.maybe_blob_store.is_some(),
      push,
      media,
      schedules: self.schedules,
      oidc_sync_last_success: state.metrics.job_last_success(BackgroundJob::OidcSync),
      oidc_sync_held: state.oidc_sync.held(),
      retention_last_success: state.metrics.job_last_success(BackgroundJob::Retention),
      partition_last_success: state
        .metrics
        .job_last_success(BackgroundJob::PartitionMaintenance),
      holiday_resync_last_success: state.metrics.job_last_success(BackgroundJob::HolidayResync),
      heartbeats,
      holiday_feed: state.holiday_feed.health(&state.settings.get().holidays),
    }
  }

  fn policy(&self, settings: &HealthAlertSettings) -> AlertPolicy {
    let seconds = |secs: u64| Duration::seconds(i64::try_from(secs).unwrap_or(i64::MAX));
    AlertPolicy {
      is_enabled: settings.is_enabled,
      hold: seconds(settings.hold_secs),
      clear: seconds(settings.clear_secs),
      max_gap: seconds(self.config.check_interval().as_secs()) * GAP_INTERVALS,
    }
  }

  /// Sends the gateway event to the admins and owners, wherever they are signed in.
  async fn tell_admins(&self, state: &AppState) {
    let admins = self.read_admins(state).await;
    state.hub.publish(admins, &GatewayEvent::HealthAlertUpdate);
  }

  async fn read_admins(&self, state: &AppState) -> Vec<UserId> {
    match state.database.list_active_admin_ids().await {
      Ok(admins) => {
        self.lock().admins.clone_from(&admins);
        admins
      },
      Err(error) => {
        warn!(%error, "reading the admins for a health alert failed: using the last known");
        self.lock().admins.clone()
      },
    }
  }

  fn lock(&self) -> MutexGuard<'_, Inner> {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Evaluates the checks until the process ends.
pub fn spawn(state: AppState) {
  tokio::spawn(async move {
    let mut interval = tokio::time::interval(state.health.config.check_interval());
    // A late tick is not made up for: the checks look at the state now.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
      interval.tick().await;
      state.health.evaluate(&state, OffsetDateTime::now_utc()).await;
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_checks_run_at_least_every_few_seconds_whatever_the_config_says() {
    let config = HealthConfig {
      check_interval_secs: 0,
      ..HealthConfig::default()
    };
    assert_eq!(
      config.check_interval(),
      StdDuration::from_secs(MIN_CHECK_INTERVAL_SECS)
    );
  }

  #[test]
  fn the_defaults_run_every_half_minute_and_report_nothing_without_a_directory() {
    let config = HealthConfig::default();
    assert_eq!(config.check_interval(), StdDuration::from_secs(30));
    assert!(config.maybe_heartbeat_dir.is_none());
  }
}
