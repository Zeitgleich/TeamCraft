use teamcraft_proto::{DeviceId, Snowflake};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::to_db;

/// A key package taken from a device.
#[derive(Debug, Clone)]
pub struct ClaimedPackage {
  pub key_package: Vec<u8>,
  pub is_last_resort: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPackageCounts {
  pub available: i64,
  pub has_last_resort: bool,
}

/// What `add_key_packages` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddPackagesOutcome {
  Stored,
  /// The device would exceed its limit of unused packages.
  OverLimit,
}

impl Database {
  /// Stores regular packages and replaces the last-resort package, in one transaction.
  /// `ids` supplies one fresh ID per regular package plus one for the last-resort package.
  pub async fn add_key_packages(
    &self,
    device: DeviceId,
    regular: &[(Snowflake, &[u8])],
    maybe_last_resort: Option<(Snowflake, &[u8])>,
    max_available: usize,
  ) -> Result<AddPackagesOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    // Serialize concurrent uploads of one device so the limit check stays exact.
    sqlx::query!("SELECT id FROM devices WHERE id = $1 FOR UPDATE", to_db(device))
      .fetch_one(&mut *tx)
      .await?;
    let current = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM key_packages
         WHERE device_id = $1 AND consumed_at IS NULL AND NOT is_last_resort"#,
      to_db(device),
    )
    .fetch_one(&mut *tx)
    .await?;
    if usize::try_from(current)
      .unwrap_or(usize::MAX)
      .saturating_add(regular.len())
      > max_available
    {
      return Ok(AddPackagesOutcome::OverLimit);
    }

    for (id, bytes) in regular {
      sqlx::query!(
        "INSERT INTO key_packages (id, device_id, key_package, is_last_resort) VALUES ($1, $2, $3, false)",
        id.get(),
        to_db(device),
        *bytes,
      )
      .execute(&mut *tx)
      .await?;
    }
    if let Some((id, bytes)) = maybe_last_resort {
      sqlx::query!(
        "DELETE FROM key_packages WHERE device_id = $1 AND is_last_resort AND consumed_at IS NULL",
        to_db(device),
      )
      .execute(&mut *tx)
      .await?;
      sqlx::query!(
        "INSERT INTO key_packages (id, device_id, key_package, is_last_resort) VALUES ($1, $2, $3, true)",
        id.get(),
        to_db(device),
        bytes,
      )
      .execute(&mut *tx)
      .await?;
    }
    tx.commit().await?;
    Ok(AddPackagesOutcome::Stored)
  }

  pub async fn key_package_counts(&self, device: DeviceId) -> Result<KeyPackageCounts, DbError> {
    let row = sqlx::query!(
      r#"SELECT count(*) FILTER (WHERE NOT is_last_resort) AS "available!",
                count(*) FILTER (WHERE is_last_resort) > 0 AS "has_last_resort!"
         FROM key_packages WHERE device_id = $1 AND consumed_at IS NULL"#,
      to_db(device),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(KeyPackageCounts {
      available: row.available,
      has_last_resort: row.has_last_resort,
    })
  }

  /// Takes one regular package of the device, atomically (concurrent claims never get the same one).
  /// Falls back to the last-resort package, which stays available. `None` if the device has neither.
  pub async fn claim_key_package(&self, device: DeviceId) -> Result<Option<ClaimedPackage>, DbError> {
    let regular = sqlx::query_scalar!(
      r#"UPDATE key_packages SET consumed_at = now()
         WHERE id = (SELECT id FROM key_packages
                     WHERE device_id = $1 AND consumed_at IS NULL AND NOT is_last_resort
                     ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED)
         RETURNING key_package"#,
      to_db(device),
    )
    .fetch_optional(self.pool())
    .await?;
    if let Some(key_package) = regular {
      return Ok(Some(ClaimedPackage {
        key_package,
        is_last_resort: false,
      }));
    }
    let last_resort = sqlx::query_scalar!(
      "SELECT key_package FROM key_packages WHERE device_id = $1 AND is_last_resort AND consumed_at IS NULL",
      to_db(device),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(last_resort.map(|key_package| ClaimedPackage {
      key_package,
      is_last_resort: true,
    }))
  }
}
