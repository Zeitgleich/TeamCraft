//! The announcement banner.

use teamcraft_proto::{AnnouncementId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAnnouncement {
  pub id: AnnouncementId,
  pub text: String,
  pub level: String,
  pub is_dismissible: bool,
  pub created_at: OffsetDateTime,
  pub maybe_expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct NewAnnouncement<'a> {
  pub id: AnnouncementId,
  pub text: &'a str,
  pub level: &'a str,
  pub is_dismissible: bool,
  pub created_by: UserId,
  pub maybe_expires_at: Option<OffsetDateTime>,
}

impl Database {
  /// Ends the current banner (if any) and starts this one.
  pub async fn replace_announcement(&self, new: &NewAnnouncement<'_>) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!("UPDATE announcements SET ended_at = now() WHERE ended_at IS NULL")
      .execute(&mut *tx)
      .await?;
    sqlx::query!(
      r#"INSERT INTO announcements (id, text, level, is_dismissible, created_by, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)"#,
      to_db(new.id),
      new.text,
      new.level,
      new.is_dismissible,
      to_db(new.created_by),
      new.maybe_expires_at,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
  }

  /// Ends the banner. `false` if there was none.
  pub async fn end_announcement(&self) -> Result<bool, DbError> {
    let result = sqlx::query!("UPDATE announcements SET ended_at = now() WHERE ended_at IS NULL")
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected() > 0)
  }

  /// The banner in force at `now`, if any.
  pub async fn current_announcement(
    &self,
    now: OffsetDateTime,
  ) -> Result<Option<StoredAnnouncement>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, text, level, is_dismissible, created_at, expires_at FROM announcements
         WHERE ended_at IS NULL AND (expires_at IS NULL OR expires_at > $1)"#,
      now,
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(StoredAnnouncement {
          id: from_db(row.id)?,
          text: row.text,
          level: row.level,
          is_dismissible: row.is_dismissible,
          created_at: row.created_at,
          maybe_expires_at: row.expires_at,
        })
      })
      .transpose()
  }

  pub async fn is_announcement_dismissed(&self, id: AnnouncementId, user: UserId) -> Result<bool, DbError> {
    Ok(
      sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM announcement_dismissals WHERE announcement_id = $1 AND user_id = $2) AS "exists!""#,
        to_db(id),
        to_db(user),
      )
      .fetch_one(self.pool())
      .await?,
    )
  }

  pub async fn dismiss_announcement(&self, id: AnnouncementId, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
      "INSERT INTO announcement_dismissals (announcement_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
      to_db(id),
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }
}
