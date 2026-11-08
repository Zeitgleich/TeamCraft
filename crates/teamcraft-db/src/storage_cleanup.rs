//! Objects still to be deleted from the object storage (issue #743): what a delete left behind
//! because the storage didn't answer.

use crate::database::Database;
use crate::error::DbError;

/// An object whose deletion failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageCleanup {
  pub s3_key: String,
  /// Set for an unfinished upload in parts, which is aborted instead of deleted.
  pub maybe_multipart_upload_id: Option<String>,
}

impl Database {
  /// Remembers an object to delete later; an object already queued stays as it is.
  pub async fn queue_storage_cleanup(
    &self,
    s3_key: &str,
    maybe_multipart_upload_id: Option<&str>,
  ) -> Result<(), DbError> {
    sqlx::query!(
      "INSERT INTO storage_cleanup (s3_key, multipart_upload_id) VALUES ($1, $2)
       ON CONFLICT (s3_key) DO NOTHING",
      s3_key,
      maybe_multipart_upload_id,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// The longest waiting objects, at most `limit`.
  pub async fn storage_cleanups(&self, limit: i64) -> Result<Vec<StorageCleanup>, DbError> {
    let rows = sqlx::query!(
      "SELECT s3_key, multipart_upload_id FROM storage_cleanup ORDER BY queued_at, s3_key LIMIT $1",
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| StorageCleanup {
          s3_key: row.s3_key,
          maybe_multipart_upload_id: row.multipart_upload_id,
        })
        .collect(),
    )
  }

  pub async fn remove_storage_cleanup(&self, s3_key: &str) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM storage_cleanup WHERE s3_key = $1", s3_key)
      .execute(self.pool())
      .await?;
    Ok(())
  }
}
