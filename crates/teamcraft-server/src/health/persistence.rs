//! The alert history and the outbox, kept across restarts (#1646, docs/operations.md "Health and
//! alerts").
//!
//! Memory stays the source: the database may be the check that failed, and an alert about it must
//! still reach the admins. A task of its own writes what changed behind it and tries again until
//! a write works; nothing in the evaluation or the delivery waits for the database. At a start the
//! same task reads back what was kept, and writes nothing before that, so a server that starts
//! while the database is down does not overwrite what it could not read yet.
//!
//! Delivery is at least once: a notice delivered just before the process died, before its removal
//! was written, goes out once more after the restart. Everything else is sent once.

use std::collections::HashSet;
use std::time::Duration as StdDuration;

use teamcraft_db::{HealthStateChanges, HealthStateRetention, StoredHealthAlert};
use teamcraft_proto::{HealthAlertSettings, HealthCheckId};
use time::{Duration, OffsetDateTime};
use tracing::{info, warn};

use super::HealthMonitor;
use super::notices::{self, Pending};
use super::tracker::{AlertChange, AlertRecord, MAX_ALERT_RECORDS};
use crate::state::AppState;

/// One write may take this long; a hanging database is tried again with the next round.
const WRITE_TIMEOUT: StdDuration = StdDuration::from_secs(30);
/// With nothing changed, the tables are still pruned this often.
const PRUNE_EVERY: Duration = Duration::hours(6);

/// What is not written yet.
#[derive(Debug, Default)]
pub(super) struct Persistence {
  /// The history was read back after the start; nothing is written before.
  is_restored: bool,
  /// Alerts raised or resolved since the last write, by check and raise time.
  unwritten_alerts: HashSet<(HealthCheckId, OffsetDateTime)>,
  maybe_last_write: Option<OffsetDateTime>,
}

impl Persistence {
  /// Notes the alerts of one evaluation for the next write.
  pub(super) fn note(&mut self, changes: &[AlertChange]) {
    self.unwritten_alerts.extend(
      changes
        .iter()
        .map(|change| (change.check.clone(), change.raised_at)),
    );
  }
}

impl HealthMonitor {
  /// Reads the history and the outbox kept from before the start, once. Returns whether that is
  /// done; `false` while the database cannot be read, so the caller tries again.
  pub async fn restore(&self, state: &AppState) -> bool {
    if self.lock().persistence.is_restored {
      return true;
    }
    let limit = u32::try_from(MAX_ALERT_RECORDS).unwrap_or(u32::MAX);
    let kept = async {
      let alerts = state.database.health_alerts(limit).await?;
      let notices = state.database.health_notices().await?;
      Ok::<_, teamcraft_db::DbError>((alerts, notices))
    };
    let (alerts, notices) = match tokio::time::timeout(WRITE_TIMEOUT, kept).await {
      Ok(Ok(kept)) => kept,
      Ok(Err(error)) => {
        warn!(%error, "reading the kept health alerts failed; tried again later");
        return false;
      },
      Err(_) => {
        warn!("reading the kept health alerts timed out; tried again later");
        return false;
      },
    };
    let (alert_count, notice_count) = (alerts.len(), notices.len());
    {
      let mut inner = self.lock();
      let closed = inner
        .tracker
        .restore(alerts.into_iter().map(from_stored).collect());
      inner
        .persistence
        .unwritten_alerts
        .extend(closed.into_iter().map(|record| (record.check, record.raised_at)));
      inner
        .outbox
        .restore(notices.into_iter().map(Pending::from_stored).collect());
      inner.persistence.is_restored = true;
    }
    info!(
      alerts = alert_count,
      notices = notice_count,
      "health alerts kept from before the start restored"
    );
    // What was waiting is due again.
    self.wake.notify_one();
    true
  }

  /// Writes what changed since the last write and prunes the tables. Returns whether the database
  /// has everything now; what a failed write carried goes with the next one.
  pub async fn persist(&self, state: &AppState, now: OffsetDateTime) -> bool {
    let settings = state.settings.get().health_alerts.clone();
    let (changes, outbox_changes, alert_keys) = {
      let mut inner = self.lock();
      let is_prune_due = inner
        .persistence
        .maybe_last_write
        .is_none_or(|last| now - last >= PRUNE_EVERY);
      if !inner.persistence.is_restored
        || (inner.persistence.unwritten_alerts.is_empty() && !inner.outbox.has_changes() && !is_prune_due)
      {
        return inner.persistence.is_restored;
      }
      let alert_keys = std::mem::take(&mut inner.persistence.unwritten_alerts);
      let alerts = alert_keys
        .iter()
        .filter_map(|(check, raised_at)| inner.tracker.record_of(check, *raised_at))
        .map(to_stored)
        .collect();
      let outbox_changes = inner.outbox.take_changes();
      let changes = HealthStateChanges {
        alerts,
        notices: outbox_changes.changed.iter().map(Pending::to_stored).collect(),
        removed_notices: outbox_changes.removed.clone(),
      };
      (changes, outbox_changes, alert_keys)
    };
    let retention = retention(&settings, now);
    let write = state.database.save_health_state(&changes, &retention);
    let maybe_error = match tokio::time::timeout(WRITE_TIMEOUT, write).await {
      Ok(Ok(())) => None,
      Ok(Err(error)) => Some(error.to_string()),
      Err(_) => Some("timed out".to_owned()),
    };
    let mut inner = self.lock();
    match maybe_error {
      None => {
        inner.persistence.maybe_last_write = Some(now);
        true
      },
      Some(error) => {
        warn!(%error, "writing the health alerts failed; tried again later");
        inner.persistence.unwritten_alerts.extend(alert_keys);
        inner.outbox.return_changes(outbox_changes);
        false
      },
    }
  }
}

/// Reads back what was kept, then writes behind the monitor until the process ends: when told
/// that something changed, and once per check interval in case a write failed.
pub(super) fn spawn(state: AppState) {
  tokio::spawn(async move {
    let interval = state.health.config.check_interval();
    loop {
      if state.health.restore(&state).await {
        state.health.persist(&state, OffsetDateTime::now_utc()).await;
      }
      // Either is fine: a lost wake-up is found by the next tick.
      let _ = tokio::time::timeout(interval, state.health.persist_wake.notified()).await;
    }
  });
}

/// How much the tables keep under these settings.
fn retention(settings: &HealthAlertSettings, now: OffsetDateTime) -> HealthStateRetention {
  HealthStateRetention {
    resolved_before: history_start(settings, now),
    max_alerts: u32::try_from(MAX_ALERT_RECORDS).unwrap_or(u32::MAX),
    notices_queued_before: now - notices::MAX_AGE,
  }
}

/// Resolved alerts that ended before this leave the history.
pub(super) fn history_start(settings: &HealthAlertSettings, now: OffsetDateTime) -> OffsetDateTime {
  now - Duration::days(i64::from(settings.history_days.max(1)))
}

fn to_stored(record: &AlertRecord) -> StoredHealthAlert {
  StoredHealthAlert {
    check: record.check.clone(),
    raised_at: record.raised_at,
    maybe_resolved_at: record.maybe_resolved_at,
  }
}

fn from_stored(stored: StoredHealthAlert) -> AlertRecord {
  AlertRecord {
    check: stored.check,
    raised_at: stored.raised_at,
    maybe_resolved_at: stored.maybe_resolved_at,
  }
}
