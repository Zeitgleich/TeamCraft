use teamcraft_proto::{DeviceId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

#[derive(Debug, Clone)]
pub struct Device {
  pub id: DeviceId,
  pub user_id: UserId,
  pub name: String,
  pub public_key: Vec<u8>,
  pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct NewDevice<'a> {
  pub id: DeviceId,
  pub user_id: UserId,
  pub name: &'a str,
  pub public_key: &'a [u8],
  pub cross_signature: &'a [u8],
  pub maybe_push_token: Option<&'a str>,
}

impl Database {
  pub async fn register_device(&self, device: &NewDevice<'_>) -> Result<Device, DbError> {
    let row = sqlx::query!(
      r#"INSERT INTO devices (id, user_id, name, public_key, cross_signature, push_token)
         VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING id, user_id, name, public_key, created_at"#,
      to_db(device.id),
      to_db(device.user_id),
      device.name,
      device.public_key,
      device.cross_signature,
      device.maybe_push_token,
    )
    .fetch_one(self.pool())
    .await?;
    Ok(Device {
      id: from_db(row.id)?,
      user_id: from_db(row.user_id)?,
      name: row.name,
      public_key: row.public_key,
      created_at: row.created_at,
    })
  }

  /// The user's devices that haven't been revoked, oldest first.
  pub async fn list_devices(&self, user: UserId) -> Result<Vec<Device>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, user_id, name, public_key, created_at FROM devices
         WHERE user_id = $1 AND revoked_at IS NULL ORDER BY id"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(Device {
          id: from_db(row.id)?,
          user_id: from_db(row.user_id)?,
          name: row.name,
          public_key: row.public_key,
          created_at: row.created_at,
        })
      })
      .collect()
  }

  /// Revokes one of the user's devices; `false` if it doesn't exist or belongs to someone else.
  pub async fn revoke_device(&self, user: UserId, device: DeviceId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE devices SET revoked_at = now() WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
      to_db(device),
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// `true` if the device exists, is not revoked and belongs to `user`.
  pub async fn is_active_device_of(&self, user: UserId, device: DeviceId) -> Result<bool, DbError> {
    let found = sqlx::query_scalar!(
      r#"SELECT EXISTS (SELECT 1 FROM devices WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL) AS "found!""#,
      to_db(device),
      to_db(user),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(found)
  }

  pub async fn touch_device(&self, device: DeviceId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE devices SET last_seen_at = now() WHERE id = $1",
      to_db(device)
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }
}
