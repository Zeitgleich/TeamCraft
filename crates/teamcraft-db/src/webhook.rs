//! Outgoing webhooks (migration 0092, docs/webhooks.md): the configured targets, the durable
//! delivery queue and the per-webhook delivery log. A claimed delivery stays locked, together with
//! its webhook, by an open transaction while the server sends it: a crash releases the claim at
//! once, two workers never send the same row, and one webhook has one delivery in flight at a time.

use std::time::Duration;

use sqlx::{Postgres, Transaction};
use teamcraft_proto::{ConversationId, WebhookDeliveryOutcome, WebhookEventKind, WebhookId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// A webhook as stored, with its queue length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredWebhook {
  pub id: WebhookId,
  pub url: String,
  pub description: String,
  pub events: Vec<WebhookEventKind>,
  pub is_enabled: bool,
  pub created_at: OffsetDateTime,
  pub consecutive_failures: u32,
  pub maybe_paused_until: Option<OffsetDateTime>,
  pub maybe_last_success_at: Option<OffsetDateTime>,
  pub maybe_last_failure_at: Option<OffsetDateTime>,
  pub maybe_last_error: Option<String>,
  pub dropped_count: u64,
  pub queued_count: u32,
}

/// A webhook to create; the secret is sealed by the caller.
#[derive(Debug, Clone)]
pub struct NewWebhook<'a> {
  pub id: WebhookId,
  pub url: &'a str,
  pub description: &'a str,
  pub events: &'a [WebhookEventKind],
  pub is_enabled: bool,
  pub sealed_secret: &'a [u8],
}

/// The changeable fields of a webhook.
#[derive(Debug, Clone)]
pub struct WebhookChange<'a> {
  pub url: &'a str,
  pub description: &'a str,
  pub events: &'a [WebhookEventKind],
  pub is_enabled: bool,
}

/// What queueing an event did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnqueueOutcome {
  /// Webhooks the event was queued for.
  pub queued: u64,
  /// Subscribed webhooks whose queue was full; the event was dropped for them.
  pub dropped: u64,
}

/// A due delivery, as a worker sees it.
#[derive(Debug, Clone)]
pub struct WebhookDeliveryRow {
  pub webhook_id: WebhookId,
  pub event_id: i64,
  pub event_kind: WebhookEventKind,
  pub payload: String,
  /// Attempts made before this one.
  pub attempts: u32,
  pub url: String,
  pub sealed_secret: Vec<u8>,
  /// The webhook's failures in a row before this attempt (the backoff step).
  pub consecutive_failures: u32,
}

/// One attempt for the delivery log.
#[derive(Debug, Clone)]
pub struct NewWebhookLogEntry {
  pub id: i64,
  pub webhook_id: WebhookId,
  pub event_id: i64,
  pub event_kind: WebhookEventKind,
  pub attempt: u32,
  pub outcome: WebhookDeliveryOutcome,
  pub maybe_status_code: Option<u16>,
  pub maybe_error: Option<String>,
  pub duration: Duration,
}

/// One logged attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredWebhookLogEntry {
  pub event_id: i64,
  pub event_kind: WebhookEventKind,
  pub attempt: u32,
  pub outcome: WebhookDeliveryOutcome,
  pub maybe_status_code: Option<u16>,
  pub maybe_error: Option<String>,
  pub duration_ms: u32,
  pub at: OffsetDateTime,
}

/// A due delivery claimed by this worker. Its locks last until one of the finishing methods;
/// dropping the claim (or losing the connection) gives the row back unchanged.
pub struct WebhookDeliveryClaim {
  transaction: Transaction<'static, Postgres>,
  row: WebhookDeliveryRow,
}

impl Database {
  pub async fn create_webhook(&self, new: &NewWebhook<'_>) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO webhooks (id, url, description, events, is_enabled, sealed_secret)
         VALUES ($1, $2, $3, $4, $5, $6)"#,
      to_db(new.id),
      new.url,
      new.description,
      &event_names(new.events),
      new.is_enabled,
      new.sealed_secret,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Changes a webhook. Disabling it empties its queue: events of the time it was off are not
  /// sent later in a burst. A new URL empties it too: a new receiver gets no events from before it.
  /// Returns whether it exists.
  pub async fn update_webhook(&self, id: WebhookId, change: &WebhookChange<'_>) -> Result<bool, DbError> {
    let mut transaction = self.pool().begin().await?;
    let maybe_old = sqlx::query!("SELECT url FROM webhooks WHERE id = $1 FOR UPDATE", to_db(id))
      .fetch_optional(&mut *transaction)
      .await?;
    let Some(old) = maybe_old else {
      return Ok(false);
    };
    let result = sqlx::query!(
      r#"UPDATE webhooks
         SET url = $2, description = $3, events = $4, is_enabled = $5, updated_at = now(),
             consecutive_failures = CASE WHEN url = $2 THEN consecutive_failures ELSE 0 END,
             paused_until = CASE WHEN url = $2 THEN paused_until ELSE NULL END
         WHERE id = $1"#,
      to_db(id),
      change.url,
      change.description,
      &event_names(change.events),
      change.is_enabled,
    )
    .execute(&mut *transaction)
    .await?;
    if result.rows_affected() == 0 {
      return Ok(false);
    }
    if !change.is_enabled || old.url != change.url {
      sqlx::query!("DELETE FROM webhook_deliveries WHERE webhook_id = $1", to_db(id))
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(true)
  }

  /// Replaces the sealed secret. Returns whether the webhook exists.
  pub async fn replace_webhook_secret(&self, id: WebhookId, sealed_secret: &[u8]) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE webhooks SET sealed_secret = $2, updated_at = now() WHERE id = $1",
      to_db(id),
      sealed_secret,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Deletes a webhook with its queue and log. Returns whether it existed.
  pub async fn delete_webhook(&self, id: WebhookId) -> Result<bool, DbError> {
    let result = sqlx::query!("DELETE FROM webhooks WHERE id = $1", to_db(id))
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected() == 1)
  }

  pub async fn count_webhooks(&self) -> Result<u64, DbError> {
    let row = sqlx::query!(r#"SELECT COUNT(*) AS "count!" FROM webhooks"#)
      .fetch_one(self.pool())
      .await?;
    Ok(u64::try_from(row.count).unwrap_or(0))
  }

  /// All webhooks, oldest first.
  pub async fn list_webhooks(&self) -> Result<Vec<StoredWebhook>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT w.id, w.url, w.description, w.events, w.is_enabled, w.created_at,
                w.consecutive_failures, w.paused_until, w.last_success_at, w.last_failure_at,
                w.last_error, w.dropped_count,
                (SELECT COUNT(*) FROM webhook_deliveries d WHERE d.webhook_id = w.id) AS "queued!"
         FROM webhooks w ORDER BY w.id"#,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredWebhook {
          id: from_db(row.id)?,
          url: row.url,
          description: row.description,
          events: row.events.into_iter().map(WebhookEventKind::from).collect(),
          is_enabled: row.is_enabled,
          created_at: row.created_at,
          consecutive_failures: u32::try_from(row.consecutive_failures).unwrap_or(0),
          maybe_paused_until: row.paused_until,
          maybe_last_success_at: row.last_success_at,
          maybe_last_failure_at: row.last_failure_at,
          maybe_last_error: row.last_error,
          dropped_count: u64::try_from(row.dropped_count).unwrap_or(0),
          queued_count: u32::try_from(row.queued).unwrap_or(u32::MAX),
        })
      })
      .collect()
  }

  /// A webhook's URL and sealed secret, for a test delivery.
  pub async fn webhook_target(&self, id: WebhookId) -> Result<Option<(String, Vec<u8>)>, DbError> {
    let maybe_row = sqlx::query!("SELECT url, sealed_secret FROM webhooks WHERE id = $1", to_db(id))
      .fetch_optional(self.pool())
      .await?;
    Ok(maybe_row.map(|row| (row.url, row.sealed_secret)))
  }

  /// Queues an event for every enabled webhook subscribed to its kind whose queue has fewer than
  /// `max_queued` events; a full queue drops it and counts the drop (a target that is down for
  /// long must not grow the table without bound). One statement, so concurrent events see a
  /// consistent count; the bound may be passed by a few under heavy concurrency.
  pub async fn enqueue_webhook_event(
    &self,
    event_id: i64,
    kind: &WebhookEventKind,
    payload: &str,
    max_queued: u32,
  ) -> Result<EnqueueOutcome, DbError> {
    let row = sqlx::query!(
      r#"WITH targets AS (
           SELECT w.id,
                  (SELECT COUNT(*) FROM webhook_deliveries d WHERE d.webhook_id = w.id) < $4 AS has_room
           FROM webhooks w
           WHERE w.is_enabled AND $2 = ANY (w.events)
         ), inserted AS (
           INSERT INTO webhook_deliveries (webhook_id, event_id, event_kind, payload)
           SELECT id, $1, $2, $3 FROM targets WHERE has_room
           ON CONFLICT DO NOTHING
           RETURNING webhook_id
         ), dropped AS (
           UPDATE webhooks SET dropped_count = dropped_count + 1
           WHERE id IN (SELECT id FROM targets WHERE NOT has_room)
           RETURNING id
         )
         SELECT (SELECT COUNT(*) FROM inserted) AS "queued!", (SELECT COUNT(*) FROM dropped) AS "dropped!""#,
      event_id,
      kind.as_str(),
      payload,
      i64::from(max_queued),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(EnqueueOutcome {
      queued: u64::try_from(row.queued).unwrap_or(0),
      dropped: u64::try_from(row.dropped).unwrap_or(0),
    })
  }

  /// Claims the oldest delivery of an enabled webhook that is not backing off and has no delivery
  /// in flight, if any. The webhook row gets `FOR NO KEY UPDATE`, not `FOR UPDATE`: new deliveries'
  /// foreign-key checks take `FOR KEY SHARE` on it, and must not wait for the send in flight.
  pub async fn claim_webhook_delivery(&self) -> Result<Option<WebhookDeliveryClaim>, DbError> {
    let mut transaction = self.pool().begin().await?;
    let maybe_row = sqlx::query!(
      r#"SELECT d.webhook_id, d.event_id, d.event_kind, d.payload, d.attempts, w.url, w.sealed_secret,
                w.consecutive_failures
         FROM webhook_deliveries d
         JOIN webhooks w ON w.id = d.webhook_id
         WHERE w.is_enabled AND (w.paused_until IS NULL OR w.paused_until <= now())
         ORDER BY d.event_id
         LIMIT 1
         FOR UPDATE OF d SKIP LOCKED
         FOR NO KEY UPDATE OF w SKIP LOCKED"#,
    )
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = maybe_row else {
      // Nothing claimed: the empty transaction ends with the drop.
      return Ok(None);
    };
    Ok(Some(WebhookDeliveryClaim {
      transaction,
      row: WebhookDeliveryRow {
        webhook_id: from_db(row.webhook_id)?,
        event_id: row.event_id,
        event_kind: WebhookEventKind::from(row.event_kind),
        payload: row.payload,
        attempts: u32::try_from(row.attempts).unwrap_or(0),
        url: row.url,
        sealed_secret: row.sealed_secret,
        consecutive_failures: u32::try_from(row.consecutive_failures).unwrap_or(0),
      },
    }))
  }

  /// Drops deliveries queued longer than `max_age` and counts them as dropped. Rows a worker is
  /// sending are left alone. Returns how many were removed.
  pub async fn delete_expired_webhook_deliveries(&self, max_age: Duration) -> Result<u64, DbError> {
    let row = sqlx::query!(
      r#"WITH gone AS (
           DELETE FROM webhook_deliveries
           WHERE (webhook_id, event_id) IN (
             SELECT webhook_id, event_id FROM webhook_deliveries
             WHERE created_at < now() - $1::bigint * interval '1 millisecond'
             FOR UPDATE SKIP LOCKED)
           RETURNING webhook_id
         ), counted AS (
           UPDATE webhooks w SET dropped_count = w.dropped_count + g.count
           FROM (SELECT webhook_id, COUNT(*) AS count FROM gone GROUP BY webhook_id) g
           WHERE w.id = g.webhook_id
           RETURNING w.id
         )
         SELECT (SELECT COUNT(*) FROM gone) AS "count!""#,
      millis(max_age),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u64::try_from(row.count).unwrap_or(0))
  }

  /// Logs an attempt outside the queue (the console's test) and trims the log.
  pub async fn log_webhook_attempt(&self, entry: &NewWebhookLogEntry, keep: u32) -> Result<(), DbError> {
    let mut transaction = self.pool().begin().await?;
    insert_log(&mut transaction, entry, keep).await?;
    transaction.commit().await?;
    Ok(())
  }

  /// The latest `limit` attempts of a webhook, newest first.
  pub async fn list_webhook_log(
    &self,
    webhook: WebhookId,
    limit: u32,
  ) -> Result<Vec<StoredWebhookLogEntry>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT event_id, event_kind, attempt, outcome, status_code, error, duration_ms, at
         FROM webhook_delivery_log WHERE webhook_id = $1 ORDER BY id DESC LIMIT $2"#,
      to_db(webhook),
      i64::from(limit),
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| StoredWebhookLogEntry {
          event_id: row.event_id,
          event_kind: WebhookEventKind::from(row.event_kind),
          attempt: u32::try_from(row.attempt).unwrap_or(0),
          outcome: WebhookDeliveryOutcome::from(row.outcome),
          maybe_status_code: row.status_code.and_then(|code| u16::try_from(code).ok()),
          maybe_error: row.error,
          duration_ms: u32::try_from(row.duration_ms).unwrap_or(0),
          at: row.at,
        })
        .collect(),
    )
  }

  /// Whether the conversation is a channel (deleted ones included): webhooks tell only about
  /// channels, never about direct or group messages.
  pub async fn is_channel(&self, conversation: ConversationId) -> Result<bool, DbError> {
    let maybe_row = sqlx::query!(
      "SELECT kind = 'channel' AS \"is_channel!\" FROM conversations WHERE id = $1",
      to_db(conversation),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(maybe_row.is_some_and(|row| row.is_channel))
  }

  /// Deliveries queued for a webhook (tests and diagnostics).
  pub async fn count_webhook_deliveries(&self, webhook: WebhookId) -> Result<u64, DbError> {
    let row = sqlx::query!(
      r#"SELECT COUNT(*) AS "count!" FROM webhook_deliveries WHERE webhook_id = $1"#,
      to_db(webhook),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u64::try_from(row.count).unwrap_or(0))
  }
}

impl WebhookDeliveryClaim {
  pub fn row(&self) -> &WebhookDeliveryRow {
    &self.row
  }

  /// The target took it: out of the queue, the webhook is healthy again.
  pub async fn delivered(mut self, log: &NewWebhookLogEntry, keep_log: u32) -> Result<(), DbError> {
    self.delete_row().await?;
    sqlx::query!(
      r#"UPDATE webhooks
         SET consecutive_failures = 0, paused_until = NULL, last_success_at = now()
         WHERE id = $1"#,
      to_db(self.row.webhook_id),
    )
    .execute(&mut *self.transaction)
    .await?;
    insert_log(&mut self.transaction, log, keep_log).await?;
    self.transaction.commit().await?;
    Ok(())
  }

  /// A temporary failure: count the attempt and pause the whole webhook for `delay`, so the
  /// next attempt (this delivery again, it is the oldest) waits for the backoff.
  pub async fn retry_later(
    mut self,
    delay: Duration,
    error: &str,
    log: &NewWebhookLogEntry,
    keep_log: u32,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE webhook_deliveries SET attempts = attempts + 1
         WHERE webhook_id = $1 AND event_id = $2"#,
      to_db(self.row.webhook_id),
      self.row.event_id,
    )
    .execute(&mut *self.transaction)
    .await?;
    sqlx::query!(
      r#"UPDATE webhooks
         SET consecutive_failures = consecutive_failures + 1,
             paused_until = now() + $2::bigint * interval '1 millisecond',
             last_failure_at = now(), last_error = $3
         WHERE id = $1"#,
      to_db(self.row.webhook_id),
      millis(delay),
      error,
    )
    .execute(&mut *self.transaction)
    .await?;
    insert_log(&mut self.transaction, log, keep_log).await?;
    self.transaction.commit().await?;
    Ok(())
  }

  /// A failure for good (refused, or out of attempts): the event is dropped. When `delay` is
  /// given (the attempts ran out on a temporary failure), the webhook still backs off.
  pub async fn give_up(
    mut self,
    maybe_delay: Option<Duration>,
    error: &str,
    log: &NewWebhookLogEntry,
    keep_log: u32,
  ) -> Result<(), DbError> {
    self.delete_row().await?;
    sqlx::query!(
      r#"UPDATE webhooks
         SET consecutive_failures = consecutive_failures + 1,
             paused_until = CASE WHEN $2::bigint IS NULL THEN paused_until
                                 ELSE now() + $2::bigint * interval '1 millisecond' END,
             last_failure_at = now(), last_error = $3, dropped_count = dropped_count + 1
         WHERE id = $1"#,
      to_db(self.row.webhook_id),
      maybe_delay.map(millis),
      error,
    )
    .execute(&mut *self.transaction)
    .await?;
    insert_log(&mut self.transaction, log, keep_log).await?;
    self.transaction.commit().await?;
    Ok(())
  }

  async fn delete_row(&mut self) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM webhook_deliveries WHERE webhook_id = $1 AND event_id = $2",
      to_db(self.row.webhook_id),
      self.row.event_id,
    )
    .execute(&mut *self.transaction)
    .await?;
    Ok(())
  }
}

/// Writes the entry and keeps only the newest `keep` of its webhook.
async fn insert_log(
  transaction: &mut Transaction<'static, Postgres>,
  entry: &NewWebhookLogEntry,
  keep: u32,
) -> Result<(), DbError> {
  sqlx::query!(
    r#"INSERT INTO webhook_delivery_log
         (id, webhook_id, event_id, event_kind, attempt, outcome, status_code, error, duration_ms)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"#,
    entry.id,
    to_db(entry.webhook_id),
    entry.event_id,
    entry.event_kind.as_str(),
    i32::try_from(entry.attempt).unwrap_or(i32::MAX),
    entry.outcome.as_str(),
    entry.maybe_status_code.map(i32::from),
    entry.maybe_error.as_deref(),
    i32::try_from(entry.duration.as_millis()).unwrap_or(i32::MAX),
  )
  .execute(&mut **transaction)
  .await?;
  sqlx::query!(
    r#"DELETE FROM webhook_delivery_log
       WHERE webhook_id = $1
         AND id < (SELECT MIN(id) FROM (
                     SELECT id FROM webhook_delivery_log WHERE webhook_id = $1
                     ORDER BY id DESC LIMIT $2) newest)"#,
    to_db(entry.webhook_id),
    i64::from(keep.max(1)),
  )
  .execute(&mut **transaction)
  .await?;
  Ok(())
}

fn event_names(events: &[WebhookEventKind]) -> Vec<String> {
  events.iter().map(|event| event.as_str().to_owned()).collect()
}

fn millis(duration: Duration) -> i64 {
  i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}
