use teamcraft_proto::{DeviceId, Snowflake};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::to_db;

/// A key package to store, with the expiry read from the package itself.
#[derive(Debug, Clone, Copy)]
pub struct NewKeyPackage<'a> {
  pub id: Snowflake,
  pub bytes: &'a [u8],
  pub expires_at: OffsetDateTime,
}

/// A key package taken from a device.
#[derive(Debug, Clone)]
pub struct ClaimedPackage {
  pub key_package: Vec<u8>,
  pub is_last_resort: bool,
}

/// Unused packages of a device, split by whether they stay valid past `renew_before`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPackageCounts {
  /// Regular packages valid beyond the renewal window.
  pub available: i64,
  /// Regular packages that expire within the renewal window (still served while valid enough).
  pub expiring_soon: i64,
  /// A last-resort package valid beyond the renewal window.
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
  /// Stores regular packages and replaces the last-resort package, in one transaction. Only
  /// packages that are still valid at `now` count against `max_available`.
  pub async fn add_key_packages(
    &self,
    device: DeviceId,
    regular: &[NewKeyPackage<'_>],
    maybe_last_resort: Option<NewKeyPackage<'_>>,
    max_available: usize,
    now: OffsetDateTime,
  ) -> Result<AddPackagesOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    // Serialize concurrent uploads of one device so the limit check stays exact.
    sqlx::query!("SELECT id FROM devices WHERE id = $1 FOR UPDATE", to_db(device))
      .fetch_one(&mut *tx)
      .await?;
    let current = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM key_packages
         WHERE device_id = $1 AND consumed_at IS NULL AND NOT is_last_resort AND expires_at > $2"#,
      to_db(device),
      now,
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

    for package in regular {
      sqlx::query!(
        "INSERT INTO key_packages (id, device_id, key_package, is_last_resort, expires_at)
         VALUES ($1, $2, $3, false, $4)",
        package.id.get(),
        to_db(device),
        package.bytes,
        package.expires_at,
      )
      .execute(&mut *tx)
      .await?;
    }
    if let Some(package) = maybe_last_resort {
      sqlx::query!(
        "DELETE FROM key_packages WHERE device_id = $1 AND is_last_resort AND consumed_at IS NULL",
        to_db(device),
      )
      .execute(&mut *tx)
      .await?;
      sqlx::query!(
        "INSERT INTO key_packages (id, device_id, key_package, is_last_resort, expires_at)
         VALUES ($1, $2, $3, true, $4)",
        package.id.get(),
        to_db(device),
        package.bytes,
        package.expires_at,
      )
      .execute(&mut *tx)
      .await?;
    }
    tx.commit().await?;
    Ok(AddPackagesOutcome::Stored)
  }

  /// Counts the device's unused packages valid at `now`; those expiring by `renew_before` are
  /// "expiring soon".
  pub async fn key_package_counts(
    &self,
    device: DeviceId,
    now: OffsetDateTime,
    renew_before: OffsetDateTime,
  ) -> Result<KeyPackageCounts, DbError> {
    let row = sqlx::query!(
      r#"SELECT count(*) FILTER (WHERE NOT is_last_resort AND expires_at > $3) AS "available!",
                count(*) FILTER (WHERE NOT is_last_resort AND expires_at <= $3) AS "expiring_soon!",
                count(*) FILTER (WHERE is_last_resort AND expires_at > $3) > 0 AS "has_last_resort!"
         FROM key_packages WHERE device_id = $1 AND consumed_at IS NULL AND expires_at > $2"#,
      to_db(device),
      now,
      renew_before,
    )
    .fetch_one(self.pool())
    .await?;
    Ok(KeyPackageCounts {
      available: row.available,
      expiring_soon: row.expiring_soon,
      has_last_resort: row.has_last_resort,
    })
  }

  /// Takes one regular package of the device that is valid beyond `valid_until`, atomically
  /// (concurrent claims never get the same one), the one expiring first. Falls back to the
  /// last-resort package, which stays available. `None` if the device has neither.
  pub async fn claim_key_package(
    &self,
    device: DeviceId,
    valid_until: OffsetDateTime,
  ) -> Result<Option<ClaimedPackage>, DbError> {
    let regular = sqlx::query_scalar!(
      r#"UPDATE key_packages SET consumed_at = now()
         WHERE id = (SELECT id FROM key_packages
                     WHERE device_id = $1 AND consumed_at IS NULL AND NOT is_last_resort
                       AND expires_at > $2
                     ORDER BY expires_at, id LIMIT 1 FOR UPDATE SKIP LOCKED)
         RETURNING key_package"#,
      to_db(device),
      valid_until,
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
      "SELECT key_package FROM key_packages
       WHERE device_id = $1 AND is_last_resort AND consumed_at IS NULL AND expires_at > $2",
      to_db(device),
      valid_until,
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(last_resort.map(|key_package| ClaimedPackage {
      key_package,
      is_last_resort: true,
    }))
  }

  /// Deletes packages that can't be handed out any more (expiring by `valid_until`), used or not.
  /// Returns how many were deleted.
  pub async fn delete_expiring_key_packages(&self, valid_until: OffsetDateTime) -> Result<u64, DbError> {
    let result = sqlx::query!("DELETE FROM key_packages WHERE expires_at <= $1", valid_until)
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected())
  }
}
