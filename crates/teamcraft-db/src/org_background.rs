use teamcraft_proto::{OrgBackgroundId, UserId};

use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// A call background picture uploaded for the organization (the object in S3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgBackgroundObject {
  pub id: OrgBackgroundId,
  pub s3_key: String,
  pub size_bytes: u64,
  pub is_complete: bool,
}

impl Database {
  pub async fn create_org_background(
    &self,
    id: OrgBackgroundId,
    uploaded_by: UserId,
    s3_key: &str,
    size_bytes: u64,
  ) -> Result<(), DbError> {
    let size = i64::try_from(size_bytes).map_err(|_| DbError::InvalidStoredValue("size".into()))?;
    sqlx::query!(
      "INSERT INTO org_backgrounds (id, uploaded_by, s3_key, size_bytes) VALUES ($1, $2, $3, $4)",
      to_db(id),
      to_db(uploaded_by),
      s3_key,
      size,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn count_org_backgrounds(&self) -> Result<u64, DbError> {
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM org_backgrounds"#)
      .fetch_one(self.pool())
      .await?;
    Ok(u64::try_from(count).unwrap_or(u64::MAX))
  }

  pub async fn get_org_background(
    &self,
    id: OrgBackgroundId,
  ) -> Result<Option<OrgBackgroundObject>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, s3_key, size_bytes, completed_at IS NOT NULL AS "is_complete!"
         FROM org_backgrounds WHERE id = $1 AND deleted_at IS NULL"#,
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(OrgBackgroundObject {
          id: from_db(row.id)?,
          s3_key: row.s3_key,
          size_bytes: u64::try_from(row.size_bytes).unwrap_or(0),
          is_complete: row.is_complete,
        })
      })
      .transpose()
  }

  pub async fn complete_org_background(&self, id: OrgBackgroundId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE org_backgrounds SET completed_at = COALESCE(completed_at, now()) WHERE id = $1",
      to_db(id),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Marks the background deleted; returns its S3 key so the caller deletes the object. The row
  /// stays (and counts towards the limits) until [`Self::sweep_org_backgrounds`] removes it.
  pub async fn delete_org_background(&self, id: OrgBackgroundId) -> Result<Option<String>, DbError> {
    let key = sqlx::query_scalar!(
      "UPDATE org_backgrounds SET deleted_at = now() WHERE id = $1 AND deleted_at IS NULL RETURNING s3_key",
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(key)
  }

  /// Removes the rows deleted, or announced and never completed, before `before`; returns their
  /// S3 keys so the caller deletes the objects (an expired upload URL can't recreate them).
  pub async fn sweep_org_backgrounds(&self, before: OffsetDateTime) -> Result<Vec<String>, DbError> {
    let keys = sqlx::query_scalar!(
      "DELETE FROM org_backgrounds
       WHERE deleted_at < $1 OR (completed_at IS NULL AND created_at < $1)
       RETURNING s3_key",
      before,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(keys)
  }
}
