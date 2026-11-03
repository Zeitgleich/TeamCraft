use teamcraft_proto::{AttachmentId, ConversationId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

#[derive(Debug, Clone)]
pub struct Attachment {
  pub id: AttachmentId,
  pub maybe_uploader: Option<UserId>,
  pub maybe_conversation: Option<ConversationId>,
  pub size_bytes: u64,
  pub s3_key: String,
  pub is_complete: bool,
  pub created_at: OffsetDateTime,
  /// The storage's upload ID while an upload in parts is unfinished.
  pub maybe_multipart_upload_id: Option<String>,
}

/// An unfinished attachment the sweeper or its uploader removed: its object, or the upload in parts
/// to abort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedAttachment {
  pub s3_key: String,
  pub maybe_multipart_upload_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewAttachment<'a> {
  pub id: AttachmentId,
  pub uploader: UserId,
  pub conversation: ConversationId,
  pub size_bytes: u64,
  pub s3_key: &'a str,
  pub maybe_multipart_upload_id: Option<&'a str>,
}

impl Database {
  pub async fn create_attachment(&self, new: &NewAttachment<'_>) -> Result<(), DbError> {
    let size = i64::try_from(new.size_bytes).map_err(|_| DbError::InvalidStoredValue("size".into()))?;
    sqlx::query!(
      "INSERT INTO attachments (id, uploader_user_id, conversation_id, size_bytes, s3_key, multipart_upload_id)
       VALUES ($1, $2, $3, $4, $5, $6)",
      to_db(new.id),
      to_db(new.uploader),
      to_db(new.conversation),
      size,
      new.s3_key,
      new.maybe_multipart_upload_id,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// What the user has announced: attachments not yet uploaded and the bytes of all of them.
  pub async fn attachment_usage_of(&self, user: UserId) -> Result<(u64, u64), DbError> {
    let row = sqlx::query!(
      r#"SELECT count(*) FILTER (WHERE completed_at IS NULL) AS "pending!",
                COALESCE(sum(size_bytes), 0)::bigint AS "bytes!"
         FROM attachments WHERE uploader_user_id = $1"#,
      to_db(user),
    )
    .fetch_one(self.pool())
    .await?;
    Ok((
      u64::try_from(row.pending).unwrap_or(u64::MAX),
      u64::try_from(row.bytes).unwrap_or(u64::MAX),
    ))
  }

  pub async fn get_attachment(&self, id: AttachmentId) -> Result<Option<Attachment>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, uploader_user_id, conversation_id, size_bytes, s3_key, created_at, multipart_upload_id,
                completed_at IS NOT NULL AS "is_complete!"
         FROM attachments WHERE id = $1"#,
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(Attachment {
          id: from_db(row.id)?,
          maybe_uploader: from_db_opt(row.uploader_user_id)?,
          maybe_conversation: from_db_opt(row.conversation_id)?,
          size_bytes: u64::try_from(row.size_bytes).unwrap_or(0),
          s3_key: row.s3_key,
          is_complete: row.is_complete,
          created_at: row.created_at,
          maybe_multipart_upload_id: row.multipart_upload_id,
        })
      })
      .transpose()
  }

  /// Deletes attachments that were announced before `before` but never completed, returning what
  /// they left in the storage: their upload URLs have expired, so nothing can finish them any more.
  /// An upload in parts counts from the last part its uploader asked for, against
  /// `multipart_before`: it may be continued much later (a device that was offline).
  pub async fn sweep_unfinished_attachments(
    &self,
    before: OffsetDateTime,
    multipart_before: OffsetDateTime,
  ) -> Result<Vec<RemovedAttachment>, DbError> {
    let rows = sqlx::query!(
      "DELETE FROM attachments
       WHERE completed_at IS NULL
         AND CASE WHEN multipart_upload_id IS NULL THEN created_at < $1
                  ELSE COALESCE(last_activity_at, created_at) < $2 END
       RETURNING s3_key, multipart_upload_id",
      before,
      multipart_before,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| RemovedAttachment {
          s3_key: row.s3_key,
          maybe_multipart_upload_id: row.multipart_upload_id,
        })
        .collect(),
    )
  }

  /// The uploader gave up on an unfinished attachment: deletes it and returns what it left in the
  /// storage. `None` if there is no such unfinished attachment of `uploader`.
  pub async fn delete_unfinished_attachment(
    &self,
    id: AttachmentId,
    uploader: UserId,
  ) -> Result<Option<RemovedAttachment>, DbError> {
    let row = sqlx::query!(
      "DELETE FROM attachments WHERE id = $1 AND uploader_user_id = $2 AND completed_at IS NULL
       RETURNING s3_key, multipart_upload_id",
      to_db(id),
      to_db(uploader),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| RemovedAttachment {
      s3_key: row.s3_key,
      maybe_multipart_upload_id: row.multipart_upload_id,
    }))
  }

  /// The uploader asked for another part: the sweeper leaves the upload alone for a while longer.
  pub async fn touch_attachment(&self, id: AttachmentId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE attachments SET last_activity_at = now() WHERE id = $1 AND completed_at IS NULL",
      to_db(id),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn complete_attachment(&self, id: AttachmentId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE attachments SET completed_at = COALESCE(completed_at, now()), multipart_upload_id = NULL WHERE id = $1",
      to_db(id),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }
}
