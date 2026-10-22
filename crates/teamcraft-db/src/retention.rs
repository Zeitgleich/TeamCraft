use teamcraft_proto::{ConversationId, MessageId, SpaceId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

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

/// An attachment or index segment to delete: its object key goes to the storage first.
#[derive(Debug, Clone)]
pub struct StoredObject {
  pub id: i64,
  pub s3_key: String,
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
         FROM conversations c LEFT JOIN spaces s ON s.id = c.space_id ORDER BY c.id"#,
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

  /// Deletes messages of the conversation with IDs below `before`, except pinned ones if `keep_pinned`.
  /// Also forgets their client IDs. Returns the number of deleted messages.
  pub async fn delete_messages_before(
    &self,
    conversation: ConversationId,
    before: MessageId,
    keep_pinned: bool,
  ) -> Result<u64, DbError> {
    let mut tx = self.pool().begin().await?;
    let deleted = sqlx::query!(
      "DELETE FROM messages WHERE conversation_id = $1 AND id < $2 AND NOT ($3 AND is_pinned)",
      to_db(conversation),
      to_db(before),
      keep_pinned,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query!(
      r#"DELETE FROM message_client_ids c WHERE c.conversation_id = $1 AND c.message_id < $2
         AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.id = c.message_id AND m.conversation_id = c.conversation_id)"#,
      to_db(conversation),
      to_db(before),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(deleted)
  }

  pub async fn conversation_has_pinned(&self, conversation: ConversationId) -> Result<bool, DbError> {
    let found = sqlx::query_scalar!(
      r#"SELECT EXISTS (SELECT 1 FROM messages WHERE conversation_id = $1 AND is_pinned) AS "found!""#,
      to_db(conversation),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(found)
  }

  /// Attachments of the conversation with IDs below `before`.
  pub async fn list_attachments_before(
    &self,
    conversation: ConversationId,
    before: MessageId,
  ) -> Result<Vec<StoredObject>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, s3_key FROM attachments WHERE conversation_id = $1 AND id < $2",
      to_db(conversation),
      to_db(before),
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| StoredObject {
          id: row.id,
          s3_key: row.s3_key,
        })
        .collect(),
    )
  }

  pub async fn delete_attachment_row(&self, id: i64) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM attachments WHERE id = $1", id)
      .execute(self.pool())
      .await?;
    Ok(())
  }

  /// Search index segments that end before `cutoff`.
  pub async fn list_index_segments_before(
    &self,
    conversation: ConversationId,
    cutoff: OffsetDateTime,
  ) -> Result<Vec<StoredObject>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, s3_key FROM index_segments WHERE conversation_id = $1 AND range_end <= $2",
      to_db(conversation),
      cutoff,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| StoredObject {
          id: row.id,
          s3_key: row.s3_key,
        })
        .collect(),
    )
  }

  pub async fn delete_index_segment_row(&self, id: i64) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM index_segments WHERE id = $1", id)
      .execute(self.pool())
      .await?;
    Ok(())
  }

  /// Message ciphertext that lives in S3 and belongs to messages below `before` (for deletion).
  pub async fn list_offloaded_ciphertext_before(
    &self,
    conversation: ConversationId,
    before: MessageId,
    keep_pinned: bool,
  ) -> Result<Vec<String>, DbError> {
    let keys = sqlx::query_scalar!(
      r#"SELECT ciphertext_s3_key AS "key!" FROM messages
         WHERE conversation_id = $1 AND id < $2 AND ciphertext_s3_key IS NOT NULL AND NOT ($3 AND is_pinned)"#,
      to_db(conversation),
      to_db(before),
      keep_pinned,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(keys)
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
