use std::collections::HashMap;

use teamcraft_proto::UserId;
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};

/// A user's custom status as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredStatus {
  pub maybe_emoji: Option<String>,
  pub maybe_text: Option<String>,
  pub maybe_expires_at: Option<OffsetDateTime>,
}

impl Database {
  /// Replaces the user's custom status.
  pub async fn set_custom_status(&self, user: UserId, status: &StoredStatus) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO custom_status (user_id, emoji, text, expires_at) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE SET emoji = EXCLUDED.emoji, text = EXCLUDED.text,
                                             expires_at = EXCLUDED.expires_at"#,
      to_db(user),
      status.maybe_emoji,
      status.maybe_text,
      status.maybe_expires_at,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn clear_custom_status(&self, user: UserId) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM custom_status WHERE user_id = $1", to_db(user))
      .execute(self.pool())
      .await?;
    Ok(())
  }

  /// Statuses of the given users that have not expired at `now`.
  pub async fn get_custom_statuses(
    &self,
    users: &[UserId],
    now: OffsetDateTime,
  ) -> Result<HashMap<UserId, StoredStatus>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT user_id, emoji, text, expires_at FROM custom_status
         WHERE user_id = ANY($1) AND (expires_at IS NULL OR expires_at > $2)"#,
      &to_db_all(users),
      now,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok((
          from_db(row.user_id)?,
          StoredStatus {
            maybe_emoji: row.emoji,
            maybe_text: row.text,
            maybe_expires_at: row.expires_at,
          },
        ))
      })
      .collect()
  }

  /// Deletes statuses that expired at `now`; returns whose they were.
  pub async fn delete_expired_statuses(&self, now: OffsetDateTime) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      "DELETE FROM custom_status WHERE expires_at IS NOT NULL AND expires_at <= $1 RETURNING user_id",
      now,
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }
}
