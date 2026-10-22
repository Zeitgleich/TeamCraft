//! E-mail digest of missed mentions: the per-user setting and the scheduler's bookkeeping.

use teamcraft_proto::{MessageId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// The user's digest setting as stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoredEmailDigest {
  pub is_enabled: bool,
  /// `None`: the instance default interval.
  pub maybe_interval_minutes: Option<i32>,
}

/// A user whose digest is due to be looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueDigest {
  pub user: UserId,
  /// Mentions up to this message were looked at already.
  pub watermark: MessageId,
  pub maybe_interval_minutes: Option<i32>,
}

impl Database {
  pub async fn get_email_digest(&self, user: UserId) -> Result<StoredEmailDigest, DbError> {
    let row = sqlx::query!(
      "SELECT is_enabled, interval_minutes FROM user_email_digest WHERE user_id = $1",
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(
      row.map_or_else(StoredEmailDigest::default, |row| StoredEmailDigest {
        is_enabled: row.is_enabled,
        maybe_interval_minutes: row.interval_minutes,
      }),
    )
  }

  /// Stores the setting. Turning the digest on starts "from now": older mentions are never mailed,
  /// and the first look happens at `now`.
  pub async fn set_email_digest(
    &self,
    user: UserId,
    setting: StoredEmailDigest,
    now: OffsetDateTime,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO user_email_digest (user_id, is_enabled, interval_minutes, watermark_message_id, next_check_at)
         VALUES ($1, $2, $3, tc_snowflake_floor($4), $4)
         ON CONFLICT (user_id) DO UPDATE SET
           watermark_message_id = CASE WHEN NOT user_email_digest.is_enabled AND EXCLUDED.is_enabled
             THEN EXCLUDED.watermark_message_id ELSE user_email_digest.watermark_message_id END,
           next_check_at = CASE WHEN NOT user_email_digest.is_enabled AND EXCLUDED.is_enabled
             THEN EXCLUDED.next_check_at ELSE user_email_digest.next_check_at END,
           is_enabled = EXCLUDED.is_enabled,
           interval_minutes = EXCLUDED.interval_minutes,
           updated_at = now()"#,
      to_db(user),
      setting.is_enabled,
      setting.maybe_interval_minutes,
      now,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Active users with a digest whose next look is due.
  pub async fn list_due_email_digests(
    &self,
    now: OffsetDateTime,
    limit: i64,
  ) -> Result<Vec<DueDigest>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT d.user_id, d.watermark_message_id, d.interval_minutes
         FROM user_email_digest d JOIN users u ON u.id = d.user_id
         WHERE d.is_enabled AND d.next_check_at <= $1 AND u.deactivated_at IS NULL
         ORDER BY d.next_check_at
         LIMIT $2"#,
      now,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(DueDigest {
          user: from_db(row.user_id)?,
          watermark: from_db(row.watermark_message_id)?,
          maybe_interval_minutes: row.interval_minutes,
        })
      })
      .collect()
  }

  /// Mentions of the user in `(after, upto]` that are still unread, in conversations that aren't
  /// muted. Thread replies count against the thread's read position when the user follows it.
  /// Messages of the user themselves and from before they joined don't count.
  pub async fn count_missed_mentions(
    &self,
    user: UserId,
    after: MessageId,
    upto: MessageId,
    now: OffsetDateTime,
  ) -> Result<u32, DbError> {
    let count = sqlx::query_scalar!(
      r#"SELECT COUNT(*) AS "count!"
         FROM messages m
         JOIN memberships ms ON ms.conversation_id = m.conversation_id AND ms.user_id = $1
         LEFT JOIN threads_followed tf ON tf.conversation_id = m.conversation_id
           AND tf.thread_id = m.thread_id AND tf.user_id = $1 AND tf.state = 'following'
         WHERE $1 = ANY(m.mentioned_user_ids)
           AND m.sender_user_id <> $1
           AND m.id > $2 AND m.id <= $3
           AND m.id >= tc_snowflake_floor(ms.joined_at)
           AND m.id > COALESCE(CASE WHEN m.thread_id IS NULL THEN ms.last_read_message_id
                                    ELSE COALESCE(tf.last_read_message_id, ms.last_read_message_id) END, 0)
           AND ms.notification_level <> 'none'
           AND (ms.muted_until IS NULL OR ms.muted_until <= $4)"#,
      to_db(user),
      to_db(after),
      to_db(upto),
      now,
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
  }

  /// Records a look: mentions up to `watermark` are dealt with, the next look is at `next_check_at`.
  pub async fn finish_email_digest_check(
    &self,
    user: UserId,
    watermark: MessageId,
    next_check_at: OffsetDateTime,
    maybe_sent_at: Option<OffsetDateTime>,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE user_email_digest SET
           watermark_message_id = GREATEST(watermark_message_id, $2),
           next_check_at = $3,
           last_sent_at = COALESCE($4, last_sent_at)
         WHERE user_id = $1"#,
      to_db(user),
      to_db(watermark),
      next_check_at,
      maybe_sent_at,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }
}
