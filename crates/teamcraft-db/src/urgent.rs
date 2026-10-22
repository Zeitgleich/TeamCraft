//! Acknowledgements of urgent messages and the repeat schedule for urgent direct messages.

use std::time::Duration;

use teamcraft_proto::{ConversationId, MessageId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// Somebody confirmed having seen an urgent message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acknowledgement {
  pub user: UserId,
  pub acknowledged_at: OffsetDateTime,
}

/// A recipient who is due another push for an urgent message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueReminder {
  pub conversation: ConversationId,
  pub message: MessageId,
  pub user: UserId,
}

impl Database {
  /// Records an acknowledgement and ends the user's reminders for the message. Returns `false` if
  /// the user had acknowledged before.
  pub async fn acknowledge_message(
    &self,
    conversation: ConversationId,
    message: MessageId,
    user: UserId,
  ) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let inserted = sqlx::query!(
      r#"INSERT INTO message_acknowledgements (message_id, conversation_id, user_id)
         VALUES ($1, $2, $3) ON CONFLICT DO NOTHING"#,
      to_db(message),
      to_db(conversation),
      to_db(user),
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
      "DELETE FROM urgent_reminders WHERE message_id = $1 AND user_id = $2",
      to_db(message),
      to_db(user),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(inserted.rows_affected() > 0)
  }

  pub async fn list_acknowledgements(
    &self,
    conversation: ConversationId,
    message: MessageId,
  ) -> Result<Vec<Acknowledgement>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT user_id, acknowledged_at FROM message_acknowledgements
         WHERE conversation_id = $1 AND message_id = $2 ORDER BY acknowledged_at, user_id"#,
      to_db(conversation),
      to_db(message),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(Acknowledgement {
          user: from_db(row.user_id)?,
          acknowledged_at: row.acknowledged_at,
        })
      })
      .collect()
  }

  /// Starts the repeat schedule of an urgent direct message for everybody but the sender. The first
  /// push happens right away, so the first repeat is due at `first_repeat_at`.
  pub async fn schedule_urgent_reminders(
    &self,
    conversation: ConversationId,
    message: MessageId,
    sender: UserId,
    first_repeat_at: OffsetDateTime,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO urgent_reminders (message_id, conversation_id, user_id, pushes_sent, next_push_at)
         SELECT $1, conversation_id, user_id, 1, $4 FROM memberships
         WHERE conversation_id = $2 AND user_id <> $3
         ON CONFLICT DO NOTHING"#,
      to_db(message),
      to_db(conversation),
      to_db(sender),
      first_repeat_at,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Claims up to `limit` reminders that are due at `now`: each counts as one more push attempt,
  /// the next one is due after `interval`, and a reminder that reached `max_pushes` is removed
  /// (it is still returned this once). Concurrent servers claim different rows.
  pub async fn claim_due_reminders(
    &self,
    now: OffsetDateTime,
    interval: Duration,
    max_pushes: i32,
    limit: i64,
  ) -> Result<Vec<DueReminder>, DbError> {
    let mut tx = self.pool().begin().await?;
    let rows = sqlx::query!(
      r#"UPDATE urgent_reminders r
         SET pushes_sent = r.pushes_sent + 1,
             next_push_at = $1 + make_interval(secs => $2)
         WHERE (r.message_id, r.user_id) IN (
           SELECT message_id, user_id FROM urgent_reminders
           WHERE next_push_at <= $1 ORDER BY next_push_at LIMIT $3 FOR UPDATE SKIP LOCKED)
         RETURNING r.conversation_id, r.message_id, r.user_id, r.pushes_sent"#,
      now,
      interval.as_secs_f64(),
      limit,
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM urgent_reminders WHERE pushes_sent >= $1", max_pushes)
      .execute(&mut *tx)
      .await?;
    tx.commit().await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(DueReminder {
          conversation: from_db(row.conversation_id)?,
          message: from_db(row.message_id)?,
          user: from_db(row.user_id)?,
        })
      })
      .collect()
  }
}
