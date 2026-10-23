//! Settings changed in the admin console: the mail and calendar servers, the groups of instance
//! settings, and brands with their images. Values are JSON text; the server owns their shape.

use teamcraft_proto::UserId;
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::to_db;

/// One stored setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSetting {
  pub name: String,
  /// JSON text.
  pub value: String,
  /// Encrypted with the server's settings key; never plaintext.
  pub maybe_sealed_secret: Option<Vec<u8>>,
  pub updated_at: OffsetDateTime,
}

/// A brand created or taken over in the console.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBrand {
  pub id: String,
  pub is_default: bool,
  /// JSON text.
  pub settings: String,
  pub updated_at: OffsetDateTime,
}

/// An image of a stored brand; the bytes are in the object storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBrandAsset {
  pub brand_id: String,
  pub slot: String,
  pub object_key: String,
  pub content_type: String,
  pub size_bytes: i64,
}

impl Database {
  pub async fn server_settings(&self) -> Result<Vec<StoredSetting>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT name, value::text AS "value!", sealed_secret, updated_at FROM server_settings ORDER BY name"#
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| StoredSetting {
          name: row.name,
          value: row.value,
          maybe_sealed_secret: row.sealed_secret,
          updated_at: row.updated_at,
        })
        .collect(),
    )
  }

  pub async fn server_setting(&self, name: &str) -> Result<Option<StoredSetting>, DbError> {
    let row = sqlx::query!(
      r#"SELECT name, value::text AS "value!", sealed_secret, updated_at FROM server_settings WHERE name = $1"#,
      name,
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| StoredSetting {
      name: row.name,
      value: row.value,
      maybe_sealed_secret: row.sealed_secret,
      updated_at: row.updated_at,
    }))
  }

  /// Creates or replaces a setting.
  pub async fn put_server_setting(
    &self,
    name: &str,
    value: &str,
    maybe_sealed_secret: Option<&[u8]>,
    updated_by: UserId,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO server_settings (name, value, sealed_secret, updated_by, updated_at)
         VALUES ($1, $2::text::jsonb, $3, $4, now())
         ON CONFLICT (name) DO UPDATE
         SET value = EXCLUDED.value, sealed_secret = EXCLUDED.sealed_secret,
             updated_by = EXCLUDED.updated_by, updated_at = now()"#,
      name,
      value,
      maybe_sealed_secret,
      to_db(updated_by),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Removes a setting, so the configuration file's value applies again. `false` if there was none.
  pub async fn delete_server_setting(&self, name: &str) -> Result<bool, DbError> {
    let result = sqlx::query!("DELETE FROM server_settings WHERE name = $1", name)
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected() > 0)
  }

  pub async fn stored_brands(&self) -> Result<Vec<StoredBrand>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, is_default, settings::text AS "settings!", updated_at FROM brands ORDER BY id"#
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| StoredBrand {
          id: row.id,
          is_default: row.is_default,
          settings: row.settings,
          updated_at: row.updated_at,
        })
        .collect(),
    )
  }

  pub async fn stored_brand_assets(&self) -> Result<Vec<StoredBrandAsset>, DbError> {
    let rows = sqlx::query!(
      "SELECT brand_id, slot, object_key, content_type, size_bytes FROM brand_assets ORDER BY brand_id, slot"
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| StoredBrandAsset {
          brand_id: row.brand_id,
          slot: row.slot,
          object_key: row.object_key,
          content_type: row.content_type,
          size_bytes: row.size_bytes,
        })
        .collect(),
    )
  }

  /// Creates or replaces a brand. As the default it takes that role from any other stored brand.
  pub async fn put_brand(
    &self,
    id: &str,
    is_default: bool,
    settings: &str,
    updated_by: UserId,
  ) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    if is_default {
      sqlx::query!(
        "UPDATE brands SET is_default = false WHERE is_default AND id <> $1",
        id
      )
      .execute(&mut *tx)
      .await?;
    }
    sqlx::query!(
      r#"INSERT INTO brands (id, is_default, settings, updated_by, updated_at)
         VALUES ($1, $2, $3::text::jsonb, $4, now())
         ON CONFLICT (id) DO UPDATE
         SET is_default = EXCLUDED.is_default, settings = EXCLUDED.settings,
             updated_by = EXCLUDED.updated_by, updated_at = now()"#,
      id,
      is_default,
      settings,
      to_db(updated_by),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
  }

  /// Deletes a brand and its image rows; returns the object keys of its images, `None` if there
  /// was no such brand.
  pub async fn delete_brand(&self, id: &str) -> Result<Option<Vec<String>>, DbError> {
    let mut tx = self.pool().begin().await?;
    let keys = sqlx::query_scalar!("SELECT object_key FROM brand_assets WHERE brand_id = $1", id)
      .fetch_all(&mut *tx)
      .await?;
    let deleted = sqlx::query!("DELETE FROM brands WHERE id = $1", id)
      .execute(&mut *tx)
      .await?;
    tx.commit().await?;
    Ok((deleted.rows_affected() > 0).then_some(keys))
  }

  /// Sets the image of a slot; returns the object key of the image it replaces.
  pub async fn put_brand_asset(&self, asset: &StoredBrandAsset) -> Result<Option<String>, DbError> {
    let mut tx = self.pool().begin().await?;
    let previous = sqlx::query_scalar!(
      "SELECT object_key FROM brand_assets WHERE brand_id = $1 AND slot = $2 FOR UPDATE",
      asset.brand_id,
      asset.slot,
    )
    .fetch_optional(&mut *tx)
    .await?;
    sqlx::query!(
      r#"INSERT INTO brand_assets (brand_id, slot, object_key, content_type, size_bytes, updated_at)
         VALUES ($1, $2, $3, $4, $5, now())
         ON CONFLICT (brand_id, slot) DO UPDATE
         SET object_key = EXCLUDED.object_key, content_type = EXCLUDED.content_type,
             size_bytes = EXCLUDED.size_bytes, updated_at = now()"#,
      asset.brand_id,
      asset.slot,
      asset.object_key,
      asset.content_type,
      asset.size_bytes,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(previous)
  }

  /// Removes the image of a slot; returns its object key, `None` if there was none.
  pub async fn delete_brand_asset(&self, brand_id: &str, slot: &str) -> Result<Option<String>, DbError> {
    let key = sqlx::query_scalar!(
      "DELETE FROM brand_assets WHERE brand_id = $1 AND slot = $2 RETURNING object_key",
      brand_id,
      slot,
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(key)
  }
}
