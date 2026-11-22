//! Sticker packs of the workspace (issue #2132): the signed document of each pack, the pictures
//! held for packs, and which pictures each pack's current version names.

use teamcraft_proto::{
  CustomEmojiFormat, CustomEmojiHash, StickerPackId, StickerPackKey, StickerPictureId, UserId,
};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

/// One pack as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredStickerPack {
  pub id: StickerPackId,
  pub pack: StickerPackKey,
  pub version: u64,
  pub name: String,
  pub payload: Vec<u8>,
  pub signature: Vec<u8>,
  pub maybe_updated_by: Option<UserId>,
  pub updated_at: OffsetDateTime,
}

/// One picture as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredStickerPicture {
  pub id: StickerPictureId,
  pub sha256: CustomEmojiHash,
  pub format: CustomEmojiFormat,
  pub s3_key: String,
  pub size_bytes: u64,
  pub width: u32,
  pub height: u32,
}

/// A new picture; the caller checked it.
#[derive(Debug, Clone)]
pub struct NewStickerPicture<'a> {
  pub id: StickerPictureId,
  pub sha256: CustomEmojiHash,
  pub format: &'a CustomEmojiFormat,
  pub s3_key: &'a str,
  pub size_bytes: u64,
  pub width: u32,
  pub height: u32,
  pub uploaded_by: UserId,
}

/// A pack's new version; the caller checked the document.
#[derive(Debug, Clone)]
pub struct SaveStickerPack<'a> {
  /// The row's ID if the pack is new.
  pub new_id: StickerPackId,
  pub pack: StickerPackKey,
  pub version: u64,
  pub name: &'a str,
  pub payload: &'a [u8],
  pub signature: &'a [u8],
  pub updated_by: UserId,
  /// The pictures the version names, each once.
  pub pictures: &'a [CustomEmojiHash],
}

/// What saving a pack ran into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickerPackWrite {
  Created,
  Updated,
  /// The stored version is this one or newer.
  Stale,
  /// A picture the version names isn't uploaded.
  MissingPicture,
}

impl Database {
  /// Keeps a picture; `false` when one with this hash is held already (nothing changes then).
  pub async fn create_sticker_picture(&self, picture: &NewStickerPicture<'_>) -> Result<bool, DbError> {
    let size = i64::try_from(picture.size_bytes).map_err(|_| DbError::InvalidStoredValue("size".into()))?;
    let (width, height) = (to_side(picture.width)?, to_side(picture.height)?);
    let done = sqlx::query!(
      "INSERT INTO sticker_pictures (id, sha256, format, s3_key, size_bytes, width, height, uploaded_by)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
       ON CONFLICT (sha256) DO NOTHING",
      to_db(picture.id),
      picture.sha256.as_bytes().as_slice(),
      picture.format.as_str(),
      picture.s3_key,
      size,
      width,
      height,
      to_db(picture.uploaded_by),
    )
    .execute(self.pool())
    .await?;
    Ok(done.rows_affected() == 1)
  }

  /// The picture with this hash.
  pub async fn get_sticker_picture(
    &self,
    sha256: CustomEmojiHash,
  ) -> Result<Option<StoredStickerPicture>, DbError> {
    let row = sqlx::query!(
      "SELECT id, sha256, format, s3_key, size_bytes, width, height
       FROM sticker_pictures WHERE sha256 = $1",
      sha256.as_bytes().as_slice(),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(StoredStickerPicture {
          id: from_db(row.id)?,
          sha256: hash(&row.sha256)?,
          format: CustomEmojiFormat::from(row.format),
          s3_key: row.s3_key,
          size_bytes: u64::try_from(row.size_bytes).unwrap_or(0),
          width: side(row.width)?,
          height: side(row.height)?,
        })
      })
      .transpose()
  }

  /// How many packs the workspace has.
  pub async fn count_sticker_packs(&self) -> Result<u64, DbError> {
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM sticker_packs"#)
      .fetch_one(self.pool())
      .await?;
    Ok(u64::try_from(count).unwrap_or(u64::MAX))
  }

  /// Every pack, oldest first.
  pub async fn list_sticker_packs(&self) -> Result<Vec<StoredStickerPack>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, pack_key, version, name, payload, signature, updated_by, updated_at
       FROM sticker_packs ORDER BY id"
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        stored_pack(PackRow {
          id: row.id,
          pack_key: row.pack_key,
          version: row.version,
          name: row.name,
          payload: row.payload,
          signature: row.signature,
          updated_by: row.updated_by,
          updated_at: row.updated_at,
        })
      })
      .collect()
  }

  /// One pack.
  pub async fn get_sticker_pack(&self, pack: StickerPackKey) -> Result<Option<StoredStickerPack>, DbError> {
    let row = sqlx::query!(
      "SELECT id, pack_key, version, name, payload, signature, updated_by, updated_at
       FROM sticker_packs WHERE pack_key = $1",
      pack.as_bytes().as_slice(),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        stored_pack(PackRow {
          id: row.id,
          pack_key: row.pack_key,
          version: row.version,
          name: row.name,
          payload: row.payload,
          signature: row.signature,
          updated_by: row.updated_by,
          updated_at: row.updated_at,
        })
      })
      .transpose()
  }

  /// Creates the pack or replaces it with a newer version, and records which pictures it names,
  /// in one transaction: a version that names a picture not uploaded changes nothing.
  pub async fn save_sticker_pack(&self, save: &SaveStickerPack<'_>) -> Result<StickerPackWrite, DbError> {
    let version = i64::try_from(save.version).map_err(|_| DbError::InvalidStoredValue("version".into()))?;
    let mut tx = self.pool().begin().await?;
    let maybe_current = sqlx::query!(
      "SELECT id, version FROM sticker_packs WHERE pack_key = $1 FOR UPDATE",
      save.pack.as_bytes().as_slice(),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let (pack_id, written) = match maybe_current {
      Some(current) if current.version >= version => return Ok(StickerPackWrite::Stale),
      Some(current) => {
        sqlx::query!(
          "UPDATE sticker_packs
           SET version = $2, name = $3, payload = $4, signature = $5, updated_by = $6, updated_at = now()
           WHERE id = $1",
          current.id,
          version,
          save.name,
          save.payload,
          save.signature,
          to_db(save.updated_by),
        )
        .execute(&mut *tx)
        .await?;
        sqlx::query!("DELETE FROM sticker_pack_pictures WHERE pack_id = $1", current.id)
          .execute(&mut *tx)
          .await?;
        (current.id, StickerPackWrite::Updated)
      },
      None => {
        let inserted = sqlx::query!(
          "INSERT INTO sticker_packs (id, pack_key, version, name, payload, signature, updated_by)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           ON CONFLICT (pack_key) DO NOTHING",
          to_db(save.new_id),
          save.pack.as_bytes().as_slice(),
          version,
          save.name,
          save.payload,
          save.signature,
          to_db(save.updated_by),
        )
        .execute(&mut *tx)
        .await?;
        // Created by a concurrent request since the read: that one is at least as new.
        if inserted.rows_affected() == 0 {
          return Ok(StickerPackWrite::Stale);
        }
        (to_db(save.new_id), StickerPackWrite::Created)
      },
    };
    let hashes: Vec<Vec<u8>> = save
      .pictures
      .iter()
      .map(|hash| hash.as_bytes().to_vec())
      .collect();
    let linked = sqlx::query!(
      "INSERT INTO sticker_pack_pictures (pack_id, picture_id)
       SELECT $1, id FROM sticker_pictures WHERE sha256 = ANY($2)",
      pack_id,
      &hashes,
    )
    .execute(&mut *tx)
    .await?;
    if linked.rows_affected() != save.pictures.len() as u64 {
      return Ok(StickerPackWrite::MissingPicture);
    }
    tx.commit().await?;
    Ok(written)
  }

  /// Deletes a pack; `false` when there was none.
  pub async fn delete_sticker_pack(&self, pack: StickerPackKey) -> Result<bool, DbError> {
    let done = sqlx::query!(
      "DELETE FROM sticker_packs WHERE pack_key = $1",
      pack.as_bytes().as_slice(),
    )
    .execute(self.pool())
    .await?;
    Ok(done.rows_affected() == 1)
  }

  /// Deletes the pictures no pack names that were uploaded before `before`, and returns their
  /// objects' keys for the caller to delete.
  pub async fn delete_unused_sticker_pictures(&self, before: OffsetDateTime) -> Result<Vec<String>, DbError> {
    let keys = sqlx::query_scalar!(
      "DELETE FROM sticker_pictures AS picture
       WHERE picture.uploaded_at < $1
         AND NOT EXISTS (SELECT 1 FROM sticker_pack_pictures AS used WHERE used.picture_id = picture.id)
       RETURNING picture.s3_key",
      before,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(keys)
  }
}

/// The columns of one pack row, from either query.
struct PackRow {
  id: i64,
  pack_key: Vec<u8>,
  version: i64,
  name: String,
  payload: Vec<u8>,
  signature: Vec<u8>,
  updated_by: Option<i64>,
  updated_at: OffsetDateTime,
}

fn stored_pack(row: PackRow) -> Result<StoredStickerPack, DbError> {
  let pack = <[u8; 16]>::try_from(row.pack_key.as_slice())
    .map(StickerPackKey::from_bytes)
    .map_err(|_| DbError::InvalidStoredValue("sticker pack key".into()))?;
  Ok(StoredStickerPack {
    id: from_db(row.id)?,
    pack,
    version: u64::try_from(row.version)
      .map_err(|_| DbError::InvalidStoredValue("sticker pack version".into()))?,
    name: row.name,
    payload: row.payload,
    signature: row.signature,
    maybe_updated_by: from_db_opt(row.updated_by)?,
    updated_at: row.updated_at,
  })
}

fn hash(bytes: &[u8]) -> Result<CustomEmojiHash, DbError> {
  <[u8; 32]>::try_from(bytes)
    .map(CustomEmojiHash::from_bytes)
    .map_err(|_| DbError::InvalidStoredValue("sticker picture hash".into()))
}

fn side(value: i32) -> Result<u32, DbError> {
  u32::try_from(value).map_err(|_| DbError::InvalidStoredValue("sticker picture side".into()))
}

fn to_side(side: u32) -> Result<i32, DbError> {
  i32::try_from(side).map_err(|_| DbError::InvalidStoredValue("sticker picture side".into()))
}
