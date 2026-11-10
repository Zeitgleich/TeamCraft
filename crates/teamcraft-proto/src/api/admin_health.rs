//! The system health view of the admin console and its alerts (issue #213, docs/operations.md
//! "Health and alerts"). Names, states, times and numbers only: no addresses, error texts, user or
//! conversation IDs and no content.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::UnixMillis;
use crate::wire_enum::wire_enum;

/// A quarter of a year: long enough to see what came back, short enough to stay small.
const DEFAULT_HISTORY_DAYS: u32 = 90;

wire_enum! {
  /// One thing the server watches. A fixed list: the value is shown, never built from input.
  pub enum HealthCheckId {
    /// The database answers (the breaker of the pool).
    Database => "database",
    /// The object storage for attachments answers.
    ObjectStorage => "object_storage",
    /// The identity provider answers.
    IdentityProvider => "identity_provider",
    /// The push gateway for store apps answers.
    PushGateway => "push_gateway",
    /// The push services the server sends to directly (APNs, FCM, Web Push).
    PushService => "push_service",
    /// The media server reports to the server.
    MediaServer => "media_server",
    /// The sync of users with the identity provider ran lately.
    OidcSync => "oidc_sync",
    /// The sync's safety brake holds deactivations back: admins have to look at them.
    OidcSyncBrake => "oidc_sync_brake",
    /// The retention job ran lately.
    Retention => "retention",
    /// The job that creates message partitions ahead ran lately.
    PartitionMaintenance => "partition_maintenance",
    /// The last backup is recent. Reported by the deployment (a heartbeat file).
    Backup => "backup",
    /// The served certificate is valid for long enough. Reported by the deployment.
    Certificate => "certificate",
    /// The disk is not nearly full. Reported by the deployment.
    DiskSpace => "disk_space",
    /// The copy of the holiday feed is recent enough (`holidays.feed_max_age_secs`).
    HolidayFeed => "holiday_feed",
    /// The job that writes calendar events again when the holidays change ran lately (#1441).
    HolidayResync => "holiday_resync",
  }
}

wire_enum! {
  /// How a check stands. `Failing` is not an alert yet: the check has to fail for the hold time
  /// first, so a restart or a slow answer raises nothing.
  pub enum HealthCheckStatus {
    Ok => "ok",
    /// Failing for less than the hold time.
    Failing => "failing",
    /// Failing for longer than the hold time: admins have been told.
    Alert => "alert",
    /// No data yet (just started, or the deployment has not reported).
    NoData => "no_data",
    /// Not set up on this instance.
    NotConfigured => "not_configured",
  }
}

/// One entry of [`AdminHealthResponse::checks`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HealthCheckResponse {
  pub check: HealthCheckId,
  pub status: HealthCheckStatus,
  /// While `failing` or `alert`: since when the check has been failing.
  #[serde(rename = "failing_since", default, skip_serializing_if = "Option::is_none")]
  pub maybe_failing_since: Option<UnixMillis>,
  /// What the check measures, where it measures something: the age in seconds of the last backup
  /// or job run, the days until the certificate expires, the percent of the disk in use.
  #[serde(rename = "value", default, skip_serializing_if = "Option::is_none")]
  pub maybe_value: Option<i64>,
}

/// One alert that was raised since the server started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HealthAlertResponse {
  pub check: HealthCheckId,
  pub raised_at: UnixMillis,
  /// Absent while the alert is open.
  #[serde(rename = "resolved_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_resolved_at: Option<UnixMillis>,
}

/// `GET /api/v1/admin/health`: the last evaluation of the checks. Costs no query and no call to a
/// backend: the server evaluates in the background and keeps the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminHealthResponse {
  pub checks: Vec<HealthCheckResponse>,
  /// Newest first, bounded. In memory: a restart starts the list again.
  pub alerts: Vec<HealthAlertResponse>,
  /// Whether raising alerts is on (`health_alerts` settings); the checks show either way.
  pub is_alerting_enabled: bool,
  /// How long a check has to fail before it is an alert, as set.
  pub hold_secs: u64,
  /// When the server started: checks that need history have no data for a while after it.
  pub started_at: UnixMillis,
  /// When the checks were last evaluated; absent before the first evaluation.
  #[serde(rename = "checked_at", default, skip_serializing_if = "Option::is_none")]
  pub maybe_checked_at: Option<UnixMillis>,
}

impl AdminHealthResponse {
  /// How many checks are alerting.
  pub fn open_alert_count(&self) -> usize {
    self
      .checks
      .iter()
      .filter(|check| check.status == HealthCheckStatus::Alert)
      .count()
  }
}

/// When to raise alerts and what counts as too old or too full (admin console, Settings). The
/// defaults suit a single-server instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HealthAlertSettings {
  /// Off: the health view still shows the checks, nobody is told.
  pub is_enabled: bool,
  /// A check must fail this long, without a break, before it raises an alert.
  pub hold_secs: u64,
  /// An alert is resolved once its check has been fine this long.
  pub clear_secs: u64,
  /// The last backup is too old after this long.
  pub backup_max_age_secs: u64,
  /// The certificate is too close to expiry with fewer days than this.
  pub certificate_min_days: u32,
  /// The disk is too full above this percentage.
  pub disk_max_percent: u32,
  /// Raised and resolved alerts are also mailed to the owners and admins with an e-mail address,
  /// when the server can send mail. Settings saved before this existed read as on.
  #[serde(default = "default_true")]
  pub is_email_enabled: bool,
  /// Resolved alerts are kept in the history this many days (at most the last 500 alerts). Open
  /// alerts are kept until they end. Settings saved before this existed read as the default.
  #[serde(default = "default_history_days")]
  pub history_days: u32,
}

impl Default for HealthAlertSettings {
  fn default() -> Self {
    Self {
      is_enabled: true,
      hold_secs: 600,
      clear_secs: 300,
      // 36 hours: a daily backup that missed once is not yet an alert.
      backup_max_age_secs: 36 * 3600,
      certificate_min_days: 14,
      disk_max_percent: 85,
      is_email_enabled: true,
      history_days: DEFAULT_HISTORY_DAYS,
    }
  }
}

fn default_true() -> bool {
  true
}

fn default_history_days() -> u32 {
  DEFAULT_HISTORY_DAYS
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_check_this_version_does_not_know_is_kept_as_it_came() {
    let check: HealthCheckResponse =
      serde_json::from_str(r#"{"check":"tomorrow","status":"strange"}"#).unwrap();
    assert_eq!(check.check, HealthCheckId::Unknown("tomorrow".into()));
    assert_eq!(check.status, HealthCheckStatus::Unknown("strange".into()));
  }

  #[test]
  fn the_wire_form_is_stable() {
    let response = AdminHealthResponse {
      checks: vec![HealthCheckResponse {
        check: HealthCheckId::Backup,
        status: HealthCheckStatus::Alert,
        maybe_failing_since: Some(UnixMillis(1_000)),
        maybe_value: Some(172_800),
      }],
      alerts: vec![HealthAlertResponse {
        check: HealthCheckId::Backup,
        raised_at: UnixMillis(2_000),
        maybe_resolved_at: None,
      }],
      is_alerting_enabled: true,
      hold_secs: 600,
      started_at: UnixMillis(500),
      maybe_checked_at: None,
    };
    assert_eq!(
      serde_json::to_string(&response).unwrap(),
      r#"{"checks":[{"check":"backup","status":"alert","failing_since":1000,"value":172800}],"alerts":[{"check":"backup","raised_at":2000}],"is_alerting_enabled":true,"hold_secs":600,"started_at":500}"#
    );
    assert_eq!(response.open_alert_count(), 1);
  }

  #[test]
  fn settings_saved_before_the_mail_switch_read_as_mailing() {
    let settings: HealthAlertSettings = serde_json::from_str(
      r#"{"is_enabled":true,"hold_secs":600,"clear_secs":300,"backup_max_age_secs":129600,"certificate_min_days":14,"disk_max_percent":85}"#,
    )
    .unwrap();
    assert_eq!(settings, HealthAlertSettings::default());
  }
}
