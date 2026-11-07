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
  /// A file of an unsent draft (#1220): removed at this time unless its uploader sends it.
  pub maybe_draft_until: Option<OffsetDateTime>,
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
  /// Set for a file of an unsent draft: held until then (see [`Attachment::maybe_draft_until`]).
  pub maybe_draft_until: Option<OffsetDateTime>,
}

impl Database {
  pub async fn create_attachment(&self, new: &NewAttachment<'_>) -> Result<(), DbError> {
    let size = i64::try_from(new.size_bytes).map_err(|_| DbError::InvalidStoredValue("size".into()))?;
    sqlx::query!(
      "INSERT INTO attachments (id, uploader_user_id, conversation_id, size_bytes, s3_key, multipart_upload_id,
                                draft_until)
       VALUES ($1, $2, $3, $4, $5, $6, $7)",
      to_db(new.id),
      to_db(new.uploader),
      to_db(new.conversation),
      size,
      new.s3_key,
      new.maybe_multipart_upload_id,
      new.maybe_draft_until,
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
                draft_until, completed_at IS NOT NULL AS "is_complete!"
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
          maybe_draft_until: row.draft_until,
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

  /// Files of unsent drafts the user holds (#1220).
  pub async fn draft_attachment_count(&self, user: UserId) -> Result<u64, DbError> {
    let count = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM attachments WHERE uploader_user_id = $1 AND draft_until IS NOT NULL"#,
      to_db(user),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u64::try_from(count).unwrap_or(u64::MAX))
  }

  /// Deletes the files of drafts that were held until before `now` and never sent, returning what
  /// they left in the storage.
  pub async fn sweep_expired_draft_attachments(
    &self,
    now: OffsetDateTime,
  ) -> Result<Vec<RemovedAttachment>, DbError> {
    let rows = sqlx::query!(
      "DELETE FROM attachments WHERE draft_until < $1 RETURNING s3_key, multipart_upload_id",
      now,
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

  /// The uploader sends a held draft file: it stays like any sent attachment. Whether the uploader
  /// has such a complete attachment (still held, or kept already: keeping again is harmless).
  pub async fn keep_attachment(&self, id: AttachmentId, uploader: UserId) -> Result<bool, DbError> {
    let row = sqlx::query!(
      "UPDATE attachments SET draft_until = NULL
       WHERE id = $1 AND uploader_user_id = $2 AND completed_at IS NOT NULL
       RETURNING id",
      to_db(id),
      to_db(uploader),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.is_some())
  }

  /// The uploader gave up on an unfinished attachment or on a held draft file: deletes it and
  /// returns what it left in the storage. `None` if there is no such attachment of `uploader`; a
  /// complete one that was kept (sent) is never deleted here.
  pub async fn delete_unfinished_attachment(
    &self,
    id: AttachmentId,
    uploader: UserId,
  ) -> Result<Option<RemovedAttachment>, DbError> {
    let row = sqlx::query!(
      "DELETE FROM attachments
       WHERE id = $1 AND uploader_user_id = $2 AND (completed_at IS NULL OR draft_until IS NOT NULL)
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
