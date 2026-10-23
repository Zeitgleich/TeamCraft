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
}

#[derive(Debug, Clone)]
pub struct NewAttachment<'a> {
  pub id: AttachmentId,
  pub uploader: UserId,
  pub conversation: ConversationId,
  pub size_bytes: u64,
  pub s3_key: &'a str,
}

impl Database {
  pub async fn create_attachment(&self, new: &NewAttachment<'_>) -> Result<(), DbError> {
    let size = i64::try_from(new.size_bytes).map_err(|_| DbError::InvalidStoredValue("size".into()))?;
    sqlx::query!(
      "INSERT INTO attachments (id, uploader_user_id, conversation_id, size_bytes, s3_key) VALUES ($1, $2, $3, $4, $5)",
      to_db(new.id),
      to_db(new.uploader),
      to_db(new.conversation),
      size,
      new.s3_key,
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
      r#"SELECT id, uploader_user_id, conversation_id, size_bytes, s3_key, created_at,
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
        })
      })
      .transpose()
  }

  /// Deletes attachments that were announced before `before` but never completed, returning their
  /// object keys: their upload URLs have expired, so nothing can finish them any more.
  pub async fn sweep_unfinished_attachments(&self, before: OffsetDateTime) -> Result<Vec<String>, DbError> {
    let keys = sqlx::query_scalar!(
      "DELETE FROM attachments WHERE completed_at IS NULL AND created_at < $1 RETURNING s3_key",
      before,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(keys)
  }

  pub async fn complete_attachment(&self, id: AttachmentId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE attachments SET completed_at = COALESCE(completed_at, now()) WHERE id = $1",
      to_db(id),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }
}
