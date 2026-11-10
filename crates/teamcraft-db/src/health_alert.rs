//! The health alert history and the notices waiting to go out (migration 0101, #1646,
//! docs/operations.md "Health and alerts"). The server keeps both in memory as the source, since the
//! database may be what failed, and writes them here behind it; at a start it reads them back.
//! Every write is an upsert or a delete by key, so writing the same state twice changes nothing.

use teamcraft_proto::{HealthCheckId, HealthNoticeId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// One alert of one check. A check and its raise time name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHealthAlert {
  pub check: HealthCheckId,
  pub raised_at: OffsetDateTime,
  pub maybe_resolved_at: Option<OffsetDateTime>,
}

/// A raised or resolved alert on its way to one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHealthNotice {
  pub id: HealthNoticeId,
  pub check: HealthCheckId,
  pub kind: HealthNoticeKind,
  /// When the alert was raised or resolved.
  pub changed_at: OffsetDateTime,
  pub channel: HealthNoticeChannel,
  pub queued_at: OffsetDateTime,
  pub next_attempt_at: OffsetDateTime,
  /// Failed attempts in a row so far.
  pub failures: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthNoticeKind {
  Raised,
  Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthNoticeChannel {
  /// One owner's or admin's address.
  Mail { address: String },
  /// Every webhook subscribed to the event.
  Webhook,
}

/// What one write of the server's health state carries.
#[derive(Debug, Clone, Default)]
pub struct HealthStateChanges {
  /// Alerts raised or resolved since the last write.
  pub alerts: Vec<StoredHealthAlert>,
  /// Notices queued or changed since the last write.
  pub notices: Vec<StoredHealthNotice>,
  /// Notices delivered or given up since the last write.
  pub removed_notices: Vec<HealthNoticeId>,
}

/// How much history the tables keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthStateRetention {
  /// Alerts resolved before this go. Open alerts stay until they end.
  pub resolved_before: OffsetDateTime,
  /// Of the resolved alerts, at most this many newest stay.
  pub max_alerts: u32,
  /// Notices queued before this are given up.
  pub notices_queued_before: OffsetDateTime,
}

impl Database {
  /// The newest `limit` alerts, newest first.
  pub async fn health_alerts(&self, limit: u32) -> Result<Vec<StoredHealthAlert>, DbError> {
    let rows = sqlx::query!(
      "SELECT check_id, raised_at, resolved_at FROM health_alerts
       ORDER BY raised_at DESC, check_id
       LIMIT $1",
      i64::from(limit),
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| StoredHealthAlert {
          check: HealthCheckId::from(row.check_id),
          raised_at: row.raised_at,
          maybe_resolved_at: row.resolved_at,
        })
        .collect(),
    )
  }

  /// Every notice waiting, oldest first.
  pub async fn health_notices(&self) -> Result<Vec<StoredHealthNotice>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, check_id, kind, changed_at, channel, mail_address, queued_at, next_attempt_at,
              failures
       FROM health_alert_notices
       ORDER BY queued_at, id"
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredHealthNotice {
          id: from_db(row.id)?,
          check: HealthCheckId::from(row.check_id),
          kind: HealthNoticeKind::parse(&row.kind)?,
          changed_at: row.changed_at,
          channel: HealthNoticeChannel::parse(&row.channel, row.mail_address)?,
          queued_at: row.queued_at,
          next_attempt_at: row.next_attempt_at,
          failures: u32::try_from(row.failures).unwrap_or(0),
        })
      })
      .collect()
  }

  /// Writes what changed and drops what the retention no longer keeps, in one transaction: a write
  /// that fails leaves the tables as they were, and the caller writes the same changes again.
  pub async fn save_health_state(
    &self,
    changes: &HealthStateChanges,
    retention: &HealthStateRetention,
  ) -> Result<(), DbError> {
    let mut transaction = self.pool().begin().await?;
    for alert in &changes.alerts {
      sqlx::query!(
        "INSERT INTO health_alerts (check_id, raised_at, resolved_at) VALUES ($1, $2, $3)
         ON CONFLICT (check_id, raised_at) DO UPDATE SET resolved_at = EXCLUDED.resolved_at",
        alert.check.as_str(),
        alert.raised_at,
        alert.maybe_resolved_at,
      )
      .execute(&mut *transaction)
      .await?;
    }
    let removed: Vec<i64> = changes.removed_notices.iter().copied().map(to_db).collect();
    sqlx::query!("DELETE FROM health_alert_notices WHERE id = ANY($1)", &removed,)
      .execute(&mut *transaction)
      .await?;
    for notice in &changes.notices {
      let (channel, maybe_address) = notice.channel.to_db();
      sqlx::query!(
        "INSERT INTO health_alert_notices
           (id, check_id, kind, changed_at, channel, mail_address, queued_at, next_attempt_at,
            failures)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         ON CONFLICT (id) DO UPDATE SET next_attempt_at = EXCLUDED.next_attempt_at,
                                        failures = EXCLUDED.failures",
        to_db(notice.id),
        notice.check.as_str(),
        notice.kind.as_str(),
        notice.changed_at,
        channel,
        maybe_address,
        notice.queued_at,
        notice.next_attempt_at,
        i32::try_from(notice.failures).unwrap_or(i32::MAX),
      )
      .execute(&mut *transaction)
      .await?;
    }
    prune(&mut transaction, retention).await?;
    transaction.commit().await?;
    Ok(())
  }
}

async fn prune(
  transaction: &mut sqlx::Transaction<'static, sqlx::Postgres>,
  retention: &HealthStateRetention,
) -> Result<(), DbError> {
  sqlx::query!(
    "DELETE FROM health_alerts
     WHERE resolved_at IS NOT NULL
       AND (resolved_at < $1
            OR (check_id, raised_at) NOT IN (SELECT check_id, raised_at FROM health_alerts
                                             WHERE resolved_at IS NOT NULL
                                             ORDER BY raised_at DESC
                                             LIMIT $2))",
    retention.resolved_before,
    i64::from(retention.max_alerts),
  )
  .execute(&mut **transaction)
  .await?;
  sqlx::query!(
    "DELETE FROM health_alert_notices WHERE queued_at < $1",
    retention.notices_queued_before,
  )
  .execute(&mut **transaction)
  .await?;
  Ok(())
}

impl HealthNoticeKind {
  fn as_str(self) -> &'static str {
    match self {
      Self::Raised => "raised",
      Self::Resolved => "resolved",
    }
  }

  fn parse(value: &str) -> Result<Self, DbError> {
    match value {
      "raised" => Ok(Self::Raised),
      "resolved" => Ok(Self::Resolved),
      other => Err(DbError::InvalidStoredValue(format!("health notice kind {other}"))),
    }
  }
}

impl HealthNoticeChannel {
  fn to_db(&self) -> (&'static str, Option<&str>) {
    match self {
      Self::Mail { address } => ("mail", Some(address)),
      Self::Webhook => ("webhook", None),
    }
  }

  fn parse(channel: &str, maybe_address: Option<String>) -> Result<Self, DbError> {
    match (channel, maybe_address) {
      ("mail", Some(address)) => Ok(Self::Mail { address }),
      ("webhook", None) => Ok(Self::Webhook),
      (other, _) => Err(DbError::InvalidStoredValue(format!(
        "health notice channel {other}"
      ))),
    }
  }
}
