//! Share links for external download.

use teamcraft_proto::{AttachmentId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareLink {
  pub id: String,
  pub attachment: AttachmentId,
  pub created_by: UserId,
  pub created_at: OffsetDateTime,
  pub expires_at: OffsetDateTime,
  pub maybe_max_downloads: Option<u32>,
  pub is_password_protected: bool,
  pub download_count: u32,
  pub maybe_last_downloaded_at: Option<OffsetDateTime>,
  pub is_revoked: bool,
  /// The brand that made the link; `None` for links from before brands existed (any host).
  pub maybe_brand_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewShareLink<'a> {
  pub id: &'a str,
  pub attachment: AttachmentId,
  pub created_by: UserId,
  pub expires_at: OffsetDateTime,
  pub maybe_max_downloads: Option<u32>,
  pub is_password_protected: bool,
  pub maybe_brand_id: Option<&'a str>,
}

/// What a download request found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareTarget {
  pub attachment: AttachmentId,
  pub s3_key: String,
  pub size_bytes: u64,
}

impl Database {
  pub async fn create_share_link(&self, new: &NewShareLink<'_>) -> Result<ShareLink, DbError> {
    sqlx::query!(
      r#"INSERT INTO share_links (id, attachment_id, created_by, expires_at, max_downloads, is_password_protected, brand_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
      new.id,
      to_db(new.attachment),
      to_db(new.created_by),
      new.expires_at,
      new.maybe_max_downloads.map(|n| i32::try_from(n).unwrap_or(i32::MAX)),
      new.is_password_protected,
      new.maybe_brand_id,
    )
    .execute(self.pool())
    .await?;
    self
      .get_share_link(new.id)
      .await?
      .ok_or_else(|| DbError::InvalidStoredValue("share link vanished".into()))
  }

  pub async fn get_share_link(&self, id: &str) -> Result<Option<ShareLink>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, attachment_id, created_by, created_at, expires_at, max_downloads, is_password_protected,
                download_count, last_downloaded_at, brand_id, revoked_at IS NOT NULL AS "is_revoked!"
         FROM share_links WHERE id = $1"#,
      id,
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(ShareLink {
          id: row.id,
          attachment: from_db(row.attachment_id)?,
          created_by: from_db(row.created_by)?,
          created_at: row.created_at,
          expires_at: row.expires_at,
          maybe_max_downloads: row.max_downloads.map(|n| u32::try_from(n).unwrap_or(0)),
          is_password_protected: row.is_password_protected,
          download_count: u32::try_from(row.download_count).unwrap_or(0),
          maybe_last_downloaded_at: row.last_downloaded_at,
          is_revoked: row.is_revoked,
          maybe_brand_id: row.brand_id,
        })
      })
      .transpose()
  }

  /// The links the user made, newest first.
  pub async fn list_share_links_of(&self, user: UserId, limit: i64) -> Result<Vec<ShareLink>, DbError> {
    let ids = sqlx::query_scalar!(
      "SELECT id FROM share_links WHERE created_by = $1 ORDER BY created_at DESC, id LIMIT $2",
      to_db(user),
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    let mut links = Vec::with_capacity(ids.len());
    for id in ids {
      if let Some(link) = self.get_share_link(&id).await? {
        links.push(link);
      }
    }
    Ok(links)
  }

  /// Revokes the link. `false` if it does not exist, is not the user's (unless `any_owner`) or was
  /// revoked already.
  pub async fn revoke_share_link(&self, id: &str, owner: UserId, any_owner: bool) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE share_links SET revoked_at = now()
       WHERE id = $1 AND revoked_at IS NULL AND ($3 OR created_by = $2)",
      id,
      to_db(owner),
      any_owner,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// What the public page needs to know before it asks for the file; does not count.
  pub async fn find_downloadable_share(
    &self,
    id: &str,
    now: OffsetDateTime,
  ) -> Result<Option<(ShareLink, ShareTarget)>, DbError> {
    let Some(link) = self.get_share_link(id).await? else {
      return Ok(None);
    };
    let is_usable = !link.is_revoked
      && link.expires_at > now
      && link
        .maybe_max_downloads
        .is_none_or(|max| link.download_count < max);
    if !is_usable {
      return Ok(None);
    }
    let Some(attachment) = self.get_attachment(link.attachment).await? else {
      return Ok(None);
    };
    if !attachment.is_complete {
      return Ok(None);
    }
    let target = ShareTarget {
      attachment: attachment.id,
      s3_key: attachment.s3_key,
      size_bytes: attachment.size_bytes,
    };
    Ok(Some((link, target)))
  }

  /// Counts a download if the link is still good (not revoked, not expired, downloads left), in one
  /// statement so concurrent requests cannot exceed the limit. `false` if not.
  pub async fn count_share_download(&self, id: &str, now: OffsetDateTime) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE share_links
         SET download_count = download_count + 1, last_downloaded_at = $2
         WHERE id = $1 AND revoked_at IS NULL AND expires_at > $2
           AND (max_downloads IS NULL OR download_count < max_downloads)"#,
      id,
      now,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }
}
