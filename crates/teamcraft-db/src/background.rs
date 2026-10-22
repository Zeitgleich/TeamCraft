use teamcraft_proto::{BackgroundId, UserId};

use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// One of a user's own call backgrounds (the encrypted object in S3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserBackground {
  pub id: BackgroundId,
  pub owner: UserId,
  pub s3_key: String,
  pub size_bytes: u64,
  pub is_complete: bool,
}

impl Database {
  pub async fn create_user_background(
    &self,
    id: BackgroundId,
    owner: UserId,
    s3_key: &str,
    size_bytes: u64,
  ) -> Result<(), DbError> {
    let size = i64::try_from(size_bytes).map_err(|_| DbError::InvalidStoredValue("size".into()))?;
    sqlx::query!(
      "INSERT INTO user_backgrounds (id, user_id, s3_key, size_bytes) VALUES ($1, $2, $3, $4)",
      to_db(id),
      to_db(owner),
      s3_key,
      size,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// How many backgrounds the user has, uploaded or announced.
  pub async fn count_user_backgrounds(&self, owner: UserId) -> Result<u64, DbError> {
    let count = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM user_backgrounds WHERE user_id = $1"#,
      to_db(owner),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u64::try_from(count).unwrap_or(u64::MAX))
  }

  pub async fn get_user_background(&self, id: BackgroundId) -> Result<Option<UserBackground>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, user_id, s3_key, size_bytes, completed_at IS NOT NULL AS "is_complete!"
         FROM user_backgrounds WHERE id = $1 AND deleted_at IS NULL"#,
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(UserBackground {
          id: from_db(row.id)?,
          owner: from_db(row.user_id)?,
          s3_key: row.s3_key,
          size_bytes: u64::try_from(row.size_bytes).unwrap_or(0),
          is_complete: row.is_complete,
        })
      })
      .transpose()
  }

  pub async fn complete_user_background(&self, id: BackgroundId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE user_backgrounds SET completed_at = COALESCE(completed_at, now()) WHERE id = $1",
      to_db(id),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Marks the background deleted; returns its S3 key so the caller deletes the object. The row
  /// stays (and counts towards the limits) until [`Self::sweep_user_backgrounds`] removes it.
  pub async fn delete_user_background(&self, id: BackgroundId) -> Result<Option<String>, DbError> {
    let key = sqlx::query_scalar!(
      "UPDATE user_backgrounds SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL RETURNING s3_key",
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(key)
  }

  /// Removes the rows deleted, or announced and never completed, before `before`; returns their
  /// S3 keys so the caller deletes the objects (an expired upload URL can't recreate them).
  pub async fn sweep_user_backgrounds(&self, before: OffsetDateTime) -> Result<Vec<String>, DbError> {
    let keys = sqlx::query_scalar!(
      "DELETE FROM user_backgrounds
       WHERE deleted_at < $1 OR (completed_at IS NULL AND created_at < $1)
       RETURNING s3_key",
      before,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(keys)
  }
}
