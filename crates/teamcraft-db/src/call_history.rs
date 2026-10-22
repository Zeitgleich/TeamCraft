//! Call history and missed calls.

use std::collections::HashMap;

use teamcraft_proto::{CallId, ConversationId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCall {
  pub call: CallId,
  pub conversation: ConversationId,
  pub maybe_started_by: Option<UserId>,
  pub started_at: OffsetDateTime,
  pub maybe_ended_at: Option<OffsetDateTime>,
  pub was_ringing: bool,
  pub participants: Vec<UserId>,
}

/// A ringing call nobody answered, to be reported once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissedCall {
  pub conversation: ConversationId,
  pub started_by: UserId,
}

impl Database {
  pub async fn start_call_record(
    &self,
    call: CallId,
    conversation: ConversationId,
    started_by: UserId,
    started_at: OffsetDateTime,
    was_ringing: bool,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO call_history (call_id, conversation_id, started_by, started_at, was_ringing)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING"#,
      to_db(call),
      to_db(conversation),
      to_db(started_by),
      started_at,
      was_ringing,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Remembers that the user took part; anybody but the starter answers a ringing call.
  pub async fn record_call_participant(&self, call: CallId, user: UserId) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      "INSERT INTO call_history_participants (call_id, user_id)
       SELECT $1, $2 WHERE EXISTS (SELECT 1 FROM call_history WHERE call_id = $1)
       ON CONFLICT DO NOTHING",
      to_db(call),
      to_db(user),
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
      "UPDATE call_history SET was_answered = true
       WHERE call_id = $1 AND started_by IS DISTINCT FROM $2 AND NOT was_answered",
      to_db(call),
      to_db(user),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
  }

  pub async fn finish_call_record(&self, call: CallId, ended_at: OffsetDateTime) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE call_history SET ended_at = $2 WHERE call_id = $1 AND ended_at IS NULL",
      to_db(call),
      ended_at,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Claims the report of a missed call: `Some` exactly once for a ringing call nobody answered.
  pub async fn claim_missed_call(&self, call: CallId) -> Result<Option<MissedCall>, DbError> {
    let row = sqlx::query!(
      r#"UPDATE call_history SET missed_notified = true
         WHERE call_id = $1 AND was_ringing AND NOT was_answered AND NOT missed_notified
           AND started_by IS NOT NULL
         RETURNING conversation_id, started_by AS "started_by!""#,
      to_db(call),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(MissedCall {
          conversation: from_db(row.conversation_id)?,
          started_by: from_db(row.started_by)?,
        })
      })
      .transpose()
  }

  /// Calls of the conversations the user belongs to, newest first (at most `limit + 1`).
  pub async fn list_call_history(
    &self,
    user: UserId,
    maybe_before: Option<CallId>,
    limit: i64,
  ) -> Result<Vec<StoredCall>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT h.call_id, h.conversation_id, h.started_by, h.started_at, h.ended_at, h.was_ringing
         FROM call_history h
         JOIN memberships m ON m.conversation_id = h.conversation_id AND m.user_id = $1
         WHERE ($2::bigint IS NULL OR h.call_id < $2)
         ORDER BY h.call_id DESC LIMIT $3"#,
      to_db(user),
      maybe_before.map(to_db),
      limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    let ids: Vec<i64> = rows.iter().map(|row| row.call_id).collect();
    let people = sqlx::query!(
      "SELECT call_id, user_id FROM call_history_participants WHERE call_id = ANY($1) ORDER BY call_id, user_id",
      &ids,
    )
    .fetch_all(self.pool())
    .await?;
    let mut participants: HashMap<i64, Vec<UserId>> = HashMap::new();
    for person in people {
      participants
        .entry(person.call_id)
        .or_default()
        .push(from_db(person.user_id)?);
    }
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredCall {
          call: from_db(row.call_id)?,
          conversation: from_db(row.conversation_id)?,
          maybe_started_by: from_db_opt(row.started_by)?,
          started_at: row.started_at,
          maybe_ended_at: row.ended_at,
          was_ringing: row.was_ringing,
          participants: participants.remove(&row.call_id).unwrap_or_default(),
        })
      })
      .collect()
  }
}
