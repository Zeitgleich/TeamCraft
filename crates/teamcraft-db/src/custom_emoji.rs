//! Custom emoji of the workspace (issue #207): the signed document and where the picture lies.

use teamcraft_proto::{CustomEmojiFormat, CustomEmojiHash, CustomEmojiId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

/// One emoji as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCustomEmoji {
  pub id: CustomEmojiId,
  pub shortcode: String,
  pub sha256: CustomEmojiHash,
  pub format: CustomEmojiFormat,
  pub s3_key: String,
  pub size_bytes: u64,
  pub width: u32,
  pub height: u32,
  pub payload: Vec<u8>,
  pub signature: Vec<u8>,
  pub maybe_created_by: Option<UserId>,
  pub created_at: OffsetDateTime,
}

/// A new emoji; the caller checked the picture and the document.
#[derive(Debug, Clone)]
pub struct NewCustomEmoji<'a> {
  pub id: CustomEmojiId,
  pub shortcode: &'a str,
  pub sha256: CustomEmojiHash,
  pub format: &'a CustomEmojiFormat,
  pub s3_key: &'a str,
  pub size_bytes: u64,
  pub width: u32,
  pub height: u32,
  pub payload: &'a [u8],
  pub signature: &'a [u8],
  pub created_by: UserId,
}

/// What a rename or an insert ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomEmojiWrite {
  Done,
  /// Another emoji has the name.
  NameTaken,
  /// No emoji with this ID.
  NotFound,
}

impl Database {
  /// Adds an emoji; [`CustomEmojiWrite::NameTaken`] when another one has its name.
  pub async fn create_custom_emoji(&self, emoji: &NewCustomEmoji<'_>) -> Result<CustomEmojiWrite, DbError> {
    let size = i64::try_from(emoji.size_bytes).map_err(|_| DbError::InvalidStoredValue("size".into()))?;
    let (width, height) = (to_side(emoji.width)?, to_side(emoji.height)?);
    let result = sqlx::query!(
      "INSERT INTO custom_emoji
         (id, shortcode, sha256, format, s3_key, size_bytes, width, height, payload, signature, created_by)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
      to_db(emoji.id),
      emoji.shortcode,
      emoji.sha256.as_bytes().as_slice(),
      emoji.format.as_str(),
      emoji.s3_key,
      size,
      width,
      height,
      emoji.payload,
      emoji.signature,
      to_db(emoji.created_by),
    )
    .execute(self.pool())
    .await;
    match result {
      Ok(_) => Ok(CustomEmojiWrite::Done),
      Err(sqlx::Error::Database(error)) if error.is_unique_violation() => Ok(CustomEmojiWrite::NameTaken),
      Err(error) => Err(error.into()),
    }
  }

  /// How many emoji the workspace has.
  pub async fn count_custom_emoji(&self) -> Result<u64, DbError> {
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM custom_emoji"#)
      .fetch_one(self.pool())
      .await?;
    Ok(u64::try_from(count).unwrap_or(u64::MAX))
  }

  /// Every emoji, by name.
  pub async fn list_custom_emoji(&self) -> Result<Vec<StoredCustomEmoji>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, shortcode, sha256, format, s3_key, size_bytes, width, height, payload, signature,
              created_by, created_at
       FROM custom_emoji ORDER BY shortcode"
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        stored(Row {
          id: row.id,
          shortcode: row.shortcode,
          sha256: row.sha256,
          format: row.format,
          s3_key: row.s3_key,
          size_bytes: row.size_bytes,
          width: row.width,
          height: row.height,
          payload: row.payload,
          signature: row.signature,
          created_by: row.created_by,
          created_at: row.created_at,
        })
      })
      .collect()
  }

  /// One emoji.
  pub async fn get_custom_emoji(&self, id: CustomEmojiId) -> Result<Option<StoredCustomEmoji>, DbError> {
    let row = sqlx::query!(
      "SELECT id, shortcode, sha256, format, s3_key, size_bytes, width, height, payload, signature,
              created_by, created_at
       FROM custom_emoji WHERE id = $1",
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        stored(Row {
          id: row.id,
          shortcode: row.shortcode,
          sha256: row.sha256,
          format: row.format,
          s3_key: row.s3_key,
          size_bytes: row.size_bytes,
          width: row.width,
          height: row.height,
          payload: row.payload,
          signature: row.signature,
          created_by: row.created_by,
          created_at: row.created_at,
        })
      })
      .transpose()
  }

  /// A new name with its newly signed document; the picture stays.
  pub async fn rename_custom_emoji(
    &self,
    id: CustomEmojiId,
    shortcode: &str,
    payload: &[u8],
    signature: &[u8],
  ) -> Result<CustomEmojiWrite, DbError> {
    let result = sqlx::query!(
      "UPDATE custom_emoji SET shortcode = $2, payload = $3, signature = $4
       WHERE id = $1",
      to_db(id),
      shortcode,
      payload,
      signature,
    )
    .execute(self.pool())
    .await;
    match result {
      Ok(done) if done.rows_affected() == 0 => Ok(CustomEmojiWrite::NotFound),
      Ok(_) => Ok(CustomEmojiWrite::Done),
      Err(sqlx::Error::Database(error)) if error.is_unique_violation() => Ok(CustomEmojiWrite::NameTaken),
      Err(error) => Err(error.into()),
    }
  }

  /// Deletes the emoji, freeing its name; returns its object's key for the caller to delete.
  pub async fn delete_custom_emoji(&self, id: CustomEmojiId) -> Result<Option<String>, DbError> {
    let maybe_key = sqlx::query_scalar!(
      "DELETE FROM custom_emoji WHERE id = $1 RETURNING s3_key",
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(maybe_key)
  }
}

/// The columns of one row, from either query.
struct Row {
  id: i64,
  shortcode: String,
  sha256: Vec<u8>,
  format: String,
  s3_key: String,
  size_bytes: i64,
  width: i32,
  height: i32,
  payload: Vec<u8>,
  signature: Vec<u8>,
  created_by: Option<i64>,
  created_at: OffsetDateTime,
}

fn stored(row: Row) -> Result<StoredCustomEmoji, DbError> {
  let sha256 = <[u8; 32]>::try_from(row.sha256.as_slice())
    .map(CustomEmojiHash::from_bytes)
    .map_err(|_| DbError::InvalidStoredValue("custom emoji hash".into()))?;
  let side =
    |value: i32| u32::try_from(value).map_err(|_| DbError::InvalidStoredValue("custom emoji side".into()));
  Ok(StoredCustomEmoji {
    id: from_db(row.id)?,
    shortcode: row.shortcode,
    sha256,
    format: CustomEmojiFormat::from(row.format),
    s3_key: row.s3_key,
    size_bytes: u64::try_from(row.size_bytes).unwrap_or(0),
    width: side(row.width)?,
    height: side(row.height)?,
    payload: row.payload,
    signature: row.signature,
    maybe_created_by: from_db_opt(row.created_by)?,
    created_at: row.created_at,
  })
}

fn to_side(side: u32) -> Result<i32, DbError> {
  i32::try_from(side).map_err(|_| DbError::InvalidStoredValue("custom emoji side".into()))
}
