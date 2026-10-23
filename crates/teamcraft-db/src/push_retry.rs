//! The durable retry queue of push wake-ups (migration 0050, notifications.md "Mobile push
//! delivery"). A claimed row stays locked by an open transaction while the server sends it, so a
//! crash releases the claim at once and two workers never send the same row.

use std::time::Duration;

use sqlx::{Postgres, Transaction};
use teamcraft_proto::{DeviceId, PushKind, PushRetryId, PushWakeUp, UnixMillis, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

/// A wake-up that failed for a temporary reason, to be tried again.
#[derive(Debug, Clone)]
pub struct NewPushRetry {
  pub id: PushRetryId,
  pub user: UserId,
  pub device: DeviceId,
  pub is_user_endpoint: bool,
  pub wake_up: PushWakeUp,
  /// Attempts made so far (0 when the wake-up is queued before its first attempt).
  pub attempts: u32,
  /// From now until the next attempt.
  pub delay: Duration,
  /// From now until the wake-up is no use any more.
  pub ttl: Duration,
}

/// A queued wake-up that is due, as a worker sees it.
#[derive(Debug, Clone)]
pub struct PushRetryRow {
  pub id: PushRetryId,
  pub user: UserId,
  pub device: DeviceId,
  pub wake_up: PushWakeUp,
  pub attempts: u32,
  /// What is left of the wake-up's TTL.
  pub remaining_ttl: Duration,
}

/// A due row claimed by this worker. Its lock lasts until [`Self::complete`] or
/// [`Self::reschedule`]; dropping the claim (or losing the connection) gives the row back unchanged.
pub struct PushRetryClaim {
  transaction: Transaction<'static, Postgres>,
  row: PushRetryRow,
}

impl Database {
  /// Queues the wake-up unless the device already has `max_per_device` waiting (an outage must
  /// not grow the table without bound). Returns whether it was queued.
  pub async fn enqueue_push_retry(&self, retry: &NewPushRetry, max_per_device: u32) -> Result<bool, DbError> {
    let wake_up = &retry.wake_up;
    let result = sqlx::query!(
      r#"INSERT INTO push_retries
           (id, user_id, device_id, is_user_endpoint, kind, conversation_id, message_id, sender_id,
            is_urgent, thread_id, call_id, badge, sent_at_ms, attempts, next_attempt_at, expires_at)
         SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14,
                now() + $15::bigint * interval '1 millisecond',
                now() + $16::bigint * interval '1 millisecond'
         WHERE (SELECT COUNT(*) FROM push_retries WHERE device_id = $3) < $17"#,
      to_db(retry.id),
      to_db(retry.user),
      to_db(retry.device),
      retry.is_user_endpoint,
      wake_up.kind.as_str(),
      to_db(wake_up.conversation_id),
      wake_up.maybe_message_id.map(to_db),
      wake_up.maybe_sender_id.map(to_db),
      wake_up.is_urgent,
      wake_up.maybe_thread_id.map(to_db),
      wake_up.maybe_call_id.map(to_db),
      wake_up
        .maybe_badge
        .map(|badge| i32::try_from(badge).unwrap_or(i32::MAX)),
      wake_up.maybe_sent_at.map(|at| at.0),
      i32::try_from(retry.attempts).unwrap_or(i32::MAX),
      millis(retry.delay),
      millis(retry.ttl),
      i64::from(max_per_device),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Claims the most overdue wake-up of the pool (`is_user_endpoint`) that no other worker holds,
  /// if any is due and not expired.
  pub async fn claim_push_retry(&self, is_user_endpoint: bool) -> Result<Option<PushRetryClaim>, DbError> {
    let mut transaction = self.pool().begin().await?;
    let maybe_row = sqlx::query!(
      r#"SELECT id, user_id, device_id, kind, conversation_id, message_id, sender_id, is_urgent,
                thread_id, call_id, badge, sent_at_ms, attempts,
                (EXTRACT(EPOCH FROM (expires_at - now())) * 1000)::bigint AS "remaining_ms!"
         FROM push_retries
         WHERE is_user_endpoint = $1 AND next_attempt_at <= now() AND expires_at > now()
         ORDER BY next_attempt_at
         LIMIT 1
         FOR UPDATE SKIP LOCKED"#,
      is_user_endpoint,
    )
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = maybe_row else {
      // Nothing claimed: the empty transaction ends with the drop.
      return Ok(None);
    };
    let wake_up = PushWakeUp {
      kind: PushKind::from(row.kind),
      conversation_id: from_db(row.conversation_id)?,
      maybe_message_id: from_db_opt(row.message_id)?,
      maybe_sender_id: from_db_opt(row.sender_id)?,
      is_urgent: row.is_urgent,
      maybe_thread_id: from_db_opt(row.thread_id)?,
      maybe_call_id: from_db_opt(row.call_id)?,
      maybe_badge: row.badge.map(|badge| u32::try_from(badge).unwrap_or(0)),
      maybe_sent_at: row.sent_at_ms.map(UnixMillis),
    };
    Ok(Some(PushRetryClaim {
      transaction,
      row: PushRetryRow {
        id: from_db(row.id)?,
        user: from_db(row.user_id)?,
        device: from_db(row.device_id)?,
        wake_up,
        attempts: u32::try_from(row.attempts).unwrap_or(0),
        remaining_ttl: Duration::from_millis(u64::try_from(row.remaining_ms).unwrap_or(0)),
      },
    }))
  }

  /// Removes queued wake-ups whose TTL has passed. Returns how many.
  pub async fn delete_expired_push_retries(&self) -> Result<u64, DbError> {
    let result = sqlx::query!("DELETE FROM push_retries WHERE expires_at <= now()")
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected())
  }

  /// Removes what a withdrawal sent to the device makes pointless: the queued ring of the call it
  /// names, or the queued messages of the conversation (or thread) up to the message it names.
  /// Rows another worker is sending right now are left alone. Returns how many were removed.
  pub async fn delete_superseded_push_retries(
    &self,
    device: DeviceId,
    dismiss: &PushWakeUp,
  ) -> Result<u64, DbError> {
    let result = sqlx::query!(
      r#"DELETE FROM push_retries
         WHERE id IN (
           SELECT id FROM push_retries
           WHERE device_id = $1
             AND ((kind = 'call' AND call_id = $2)
               OR (kind IN ('message', 'mention') AND conversation_id = $3 AND message_id <= $4
                   AND thread_id IS NOT DISTINCT FROM $5))
           FOR UPDATE SKIP LOCKED)"#,
      to_db(device),
      dismiss.maybe_call_id.map(to_db),
      to_db(dismiss.conversation_id),
      dismiss.maybe_message_id.map(to_db),
      dismiss.maybe_thread_id.map(to_db),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected())
  }

  /// Wake-ups queued for the device (tests and diagnostics).
  pub async fn count_push_retries(&self, device: DeviceId) -> Result<u64, DbError> {
    let row = sqlx::query!(
      r#"SELECT COUNT(*) AS "count!" FROM push_retries WHERE device_id = $1"#,
      to_db(device),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u64::try_from(row.count).unwrap_or(0))
  }
}

impl PushRetryClaim {
  pub fn row(&self) -> &PushRetryRow {
    &self.row
  }

  /// The wake-up is done with (sent, or given up): it leaves the queue.
  pub async fn complete(mut self) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM push_retries WHERE id = $1", to_db(self.row.id))
      .execute(&mut *self.transaction)
      .await?;
    self.transaction.commit().await?;
    Ok(())
  }

  /// The attempt failed for a temporary reason: count it and try again after `delay`.
  pub async fn reschedule(mut self, delay: Duration) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE push_retries
         SET attempts = attempts + 1, next_attempt_at = now() + $2::bigint * interval '1 millisecond'
         WHERE id = $1"#,
      to_db(self.row.id),
      millis(delay),
    )
    .execute(&mut *self.transaction)
    .await?;
    self.transaction.commit().await?;
    Ok(())
  }
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}
