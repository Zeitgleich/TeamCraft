use sqlx::{Postgres, Transaction};
use teamcraft_proto::{ConversationId, MessageId, SpaceId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};
use crate::legal_hold::HoldEvidence;

/// A retention setting as stored: unset fields inherit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoredRetention {
  pub maybe_max_age_days: Option<i32>,
  pub maybe_max_messages: Option<i64>,
}

/// A conversation with its own setting and the one of its space.
#[derive(Debug, Clone)]
pub struct RetentionTarget {
  pub conversation: ConversationId,
  pub own: StoredRetention,
  pub space: StoredRetention,
}

/// Where retention cuts a conversation.
#[derive(Debug, Clone, Copy)]
pub struct RetentionCut {
  /// Messages and attachments with smaller IDs go.
  pub before: MessageId,
  /// Search index segments that end at or before it go.
  pub index_cutoff: OffsetDateTime,
  /// Pinned messages stay, and with them all attachments of a conversation that has any (the
  /// server can't tell which attachment a pinned message needs).
  pub keep_pinned: bool,
}

/// What retention deleted in a conversation, with the object keys still to delete from the storage.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionDeleted {
  pub messages: u64,
  /// Offloaded message ciphertext.
  pub message_keys: Vec<String>,
  pub attachment_keys: Vec<String>,
  pub index_segment_keys: Vec<String>,
}

impl Database {
  pub async fn get_space_retention(&self, space: SpaceId) -> Result<StoredRetention, DbError> {
    let row = sqlx::query!(
      "SELECT retention_max_age_days, retention_max_messages FROM spaces WHERE id = $1",
      to_db(space)
    )
    .fetch_one(self.pool())
    .await?;
    Ok(StoredRetention {
      maybe_max_age_days: row.retention_max_age_days,
      maybe_max_messages: row.retention_max_messages,
    })
  }

  pub async fn set_space_retention(&self, space: SpaceId, retention: StoredRetention) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE spaces SET retention_max_age_days = $2, retention_max_messages = $3 WHERE id = $1",
      to_db(space),
      retention.maybe_max_age_days,
      retention.maybe_max_messages,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// The conversation's own setting and its space's.
  pub async fn get_conversation_retention(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<RetentionTarget>, DbError> {
    let row = sqlx::query!(
      r#"SELECT c.id, c.retention_max_age_days, c.retention_max_messages,
                s.retention_max_age_days AS "space_age?", s.retention_max_messages AS "space_messages?"
         FROM conversations c LEFT JOIN spaces s ON s.id = c.space_id WHERE c.id = $1"#,
      to_db(conversation),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(RetentionTarget {
          conversation: from_db(row.id)?,
          own: StoredRetention {
            maybe_max_age_days: row.retention_max_age_days,
            maybe_max_messages: row.retention_max_messages,
          },
          space: StoredRetention {
            maybe_max_age_days: row.space_age,
            maybe_max_messages: row.space_messages,
          },
        })
      })
      .transpose()
  }

  pub async fn set_conversation_retention(
    &self,
    conversation: ConversationId,
    retention: StoredRetention,
  ) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE conversations SET retention_max_age_days = $2, retention_max_messages = $3 WHERE id = $1",
      to_db(conversation),
      retention.maybe_max_age_days,
      retention.maybe_max_messages,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// All conversations; the job merges the settings with the workspace default.
  pub async fn list_retention_targets(&self) -> Result<Vec<RetentionTarget>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT c.id, c.retention_max_age_days, c.retention_max_messages,
                s.retention_max_age_days AS "space_age?", s.retention_max_messages AS "space_messages?"
         FROM conversations c LEFT JOIN spaces s ON s.id = c.space_id
         WHERE c.deleted_at IS NULL ORDER BY c.id"#,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(RetentionTarget {
          conversation: from_db(row.id)?,
          own: StoredRetention {
            maybe_max_age_days: row.retention_max_age_days,
            maybe_max_messages: row.retention_max_messages,
          },
          space: StoredRetention {
            maybe_max_age_days: row.space_age,
            maybe_max_messages: row.space_messages,
          },
        })
      })
      .collect()
  }

  pub async fn set_message_pinned(
    &self,
    conversation: ConversationId,
    message: MessageId,
    is_pinned: bool,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE messages SET is_pinned = $3 WHERE id = $1 AND conversation_id = $2",
      to_db(message),
      to_db(conversation),
      is_pinned,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// ID below which the newest `keep` messages of the conversation start (messages with smaller IDs
  /// exceed the count), `None` if there are at most `keep` messages.
  pub async fn nth_newest_message_id(
    &self,
    conversation: ConversationId,
    keep: i64,
  ) -> Result<Option<MessageId>, DbError> {
    let id = sqlx::query_scalar!(
      "SELECT id FROM messages WHERE conversation_id = $1 ORDER BY id DESC OFFSET $2 LIMIT 1",
      to_db(conversation),
      keep.saturating_sub(1).max(0),
    )
    .fetch_optional(self.pool())
    .await?;
    // `id` is the keep-th newest message; the ones before it are surplus.
    id.map(from_db).transpose()
  }

  /// Deletes what is past retention in one conversation, in one transaction that first locks the
  /// conversation's row and then reads the legal holds: `is_held` gets the [`HoldEvidence`] and
  /// says whether a legal hold keeps the conversation (data-export.md §4.2). The lock conflicts with
  /// the share lock a policy append takes on the channels it newly holds, so a hold stored before the
  /// lock is seen here and one appended later waits for this commit (issue #2143). The lock doesn't
  /// conflict with sending, which only key-shares the row.
  ///
  /// `None` when the conversation is gone or held: nothing is deleted. Otherwise the object keys of
  /// the deleted rows, for the caller to delete from the storage after the commit.
  pub async fn delete_past_retention_locked<E: From<DbError>>(
    &self,
    conversation: ConversationId,
    cut: &RetentionCut,
    is_held: impl FnOnce(&HoldEvidence) -> Result<bool, E>,
  ) -> Result<Option<RetentionDeleted>, E> {
    let mut tx = self.pool().begin().await.map_err(DbError::from)?;
    let is_there = sqlx::query_scalar!(
      "SELECT id FROM conversations WHERE id = $1 AND deleted_at IS NULL FOR NO KEY UPDATE",
      to_db(conversation),
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(DbError::from)?
    .is_some();
    if !is_there || is_held(&Self::hold_evidence_in(&mut tx, &[conversation]).await?)? {
      return Ok(None);
    }
    let deleted = delete_past_retention_in(&mut tx, conversation, cut).await?;
    tx.commit().await.map_err(DbError::from)?;
    Ok(Some(deleted))
  }

  /// Drops monthly `messages` partitions that ended before `now` and are empty. Returns their names.
  pub async fn drop_empty_message_partitions(&self, now: OffsetDateTime) -> Result<Vec<String>, DbError> {
    let names = sqlx::query_scalar!(
      r#"SELECT c.relname::text AS "name!" FROM pg_inherits i
         JOIN pg_class c ON c.oid = i.inhrelid JOIN pg_class p ON p.oid = i.inhparent
         WHERE p.relname = 'messages' AND c.relname ~ '^messages_[0-9]{4}_[0-9]{2}$'"#,
    )
    .fetch_all(self.pool())
    .await?;

    let mut dropped = Vec::new();
    for name in names {
      let Some((year, month)) = parse_partition_month(&name) else {
        continue;
      };
      let Ok(first) = time::Date::from_calendar_date(year, month, 1) else {
        continue;
      };
      let end = first.midnight().assume_utc() + time::Duration::days(i64::from(month.length(year)));
      if end > now {
        continue;
      }
      // The name matched a strict pattern above, so it is safe to use as an identifier.
      let is_empty = sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!(
        "SELECT NOT EXISTS (SELECT 1 FROM {name})"
      )))
      .fetch_one(self.pool())
      .await?;
      if is_empty {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP TABLE IF EXISTS {name}")))
          .execute(self.pool())
          .await?;
        dropped.push(name);
      }
    }
    Ok(dropped)
  }

  /// Creates the monthly partitions for the month of `now` and `months_ahead` more.
  pub async fn ensure_partitions_at(&self, now: OffsetDateTime, months_ahead: u32) -> Result<(), DbError> {
    let months = i32::try_from(months_ahead.saturating_add(1)).unwrap_or(i32::MAX);
    for table in ["messages", "mls_messages"] {
      sqlx::query!(
        "SELECT tc_ensure_month_partitions($1::text, $2::timestamptz, $3::int)",
        table,
        now,
        months
      )
      .execute(self.pool())
      .await?;
    }
    Ok(())
  }
}

async fn delete_past_retention_in(
  tx: &mut Transaction<'_, Postgres>,
  conversation: ConversationId,
  cut: &RetentionCut,
) -> Result<RetentionDeleted, DbError> {
  let message_keys = sqlx::query_scalar!(
    r#"DELETE FROM messages WHERE conversation_id = $1 AND id < $2 AND NOT ($3 AND is_pinned)
       RETURNING ciphertext_s3_key AS "key?""#,
    to_db(conversation),
    to_db(cut.before),
    cut.keep_pinned,
  )
  .fetch_all(&mut **tx)
  .await?;
  let messages = message_keys.len() as u64;
  // e2ee.md §4.12.1: an anonymous poll's ballot box goes with its message.
  sqlx::query!(
    r#"DELETE FROM poll_ballot_boxes b WHERE b.conversation_id = $1 AND b.message_id < $2
       AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.id = b.message_id AND m.conversation_id = b.conversation_id)"#,
    to_db(conversation),
    to_db(cut.before),
  )
  .execute(&mut **tx)
  .await?;
  sqlx::query!(
    r#"DELETE FROM message_client_ids c WHERE c.conversation_id = $1 AND c.message_id < $2
       AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.id = c.message_id AND m.conversation_id = c.conversation_id)"#,
    to_db(conversation),
    to_db(cut.before),
  )
  .execute(&mut **tx)
  .await?;
  let attachment_keys = sqlx::query_scalar!(
    r#"DELETE FROM attachments a WHERE a.conversation_id = $1 AND a.id < $2
       AND NOT ($3 AND EXISTS (SELECT 1 FROM messages m WHERE m.conversation_id = $1 AND m.is_pinned))
       RETURNING a.s3_key"#,
    to_db(conversation),
    to_db(cut.before),
    cut.keep_pinned,
  )
  .fetch_all(&mut **tx)
  .await?;
  let index_segment_keys = sqlx::query_scalar!(
    "DELETE FROM index_segments WHERE conversation_id = $1 AND range_end <= $2 RETURNING s3_key",
    to_db(conversation),
    cut.index_cutoff,
  )
  .fetch_all(&mut **tx)
  .await?;
  Ok(RetentionDeleted {
    messages,
    message_keys: message_keys.into_iter().flatten().collect(),
    attachment_keys,
    index_segment_keys,
  })
}

fn parse_partition_month(name: &str) -> Option<(i32, time::Month)> {
  let rest = name.strip_prefix("messages_")?;
  let (year, month) = rest.split_once('_')?;
  let month = time::Month::try_from(month.parse::<u8>().ok()?).ok()?;
  Some((year.parse().ok()?, month))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn partition_names_are_parsed_strictly() {
    assert_eq!(
      parse_partition_month("messages_2026_11"),
      Some((2026, time::Month::November))
    );
    assert_eq!(parse_partition_month("messages_2026_13"), None);
    assert_eq!(parse_partition_month("messages_default"), None);
    assert_eq!(parse_partition_month("mls_messages_2026_11"), None);
  }
}
