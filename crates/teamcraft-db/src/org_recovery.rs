//! Organization recovery material (e2ee.md §6.3.1). The server stores sealed bytes only.

use teamcraft_proto::{DeviceId, RecoveryRequestId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

/// A share sealed to an admin's device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredShare {
  pub device: DeviceId,
  pub admin: UserId,
  pub key_version: u64,
  pub sealed: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareOutcome {
  Stored,
  /// Shares of this or a newer key version exist already.
  StaleVersion {
    current: u64,
  },
}

#[derive(Debug, Clone)]
pub struct NewRecoveryRequest<'a> {
  pub id: RecoveryRequestId,
  pub user: UserId,
  pub request_public_key: &'a [u8],
  pub fingerprint: &'a str,
  pub required_approvals: u32,
  pub expires_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct StoredApproval {
  pub admin: UserId,
  pub device: DeviceId,
  pub approved_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct StoredRecoveryRequest {
  pub id: RecoveryRequestId,
  pub user: UserId,
  pub request_public_key: Vec<u8>,
  pub fingerprint: String,
  pub status: String,
  pub required_approvals: u32,
  pub created_at: OffsetDateTime,
  pub expires_at: OffsetDateTime,
  pub maybe_combining_admin: Option<UserId>,
  pub maybe_combining_device: Option<DeviceId>,
  pub has_result: bool,
  pub approvals: Vec<StoredApproval>,
  pub shares_received: u32,
}

/// What an approval did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
  /// Recorded; more approvals are needed.
  Recorded,
  /// Recorded, and with it the threshold is reached: this admin's device combines the shares.
  ThresholdReached,
  AlreadyApproved,
  /// The request is not waiting for approvals.
  NotPending,
}

impl Database {
  /// Replaces all shares by the new set. `admins` pairs each device with its owner.
  pub async fn replace_org_recovery_shares(
    &self,
    key_version: u64,
    shares: &[(DeviceId, UserId, Vec<u8>)],
  ) -> Result<ShareOutcome, DbError> {
    let version =
      i64::try_from(key_version).map_err(|_| DbError::InvalidStoredValue("key version".into()))?;
    let mut tx = self.pool().begin().await?;
    let current =
      sqlx::query_scalar!(r#"SELECT COALESCE(max(key_version), 0) AS "version!" FROM org_recovery_shares"#)
        .fetch_one(&mut *tx)
        .await?;
    if version <= current {
      return Ok(ShareOutcome::StaleVersion {
        current: u64::try_from(current).unwrap_or(0),
      });
    }
    sqlx::query!("DELETE FROM org_recovery_shares")
      .execute(&mut *tx)
      .await?;
    for (device, admin, sealed) in shares {
      sqlx::query!(
        "INSERT INTO org_recovery_shares (device_id, admin_id, key_version, sealed) VALUES ($1, $2, $3, $4)",
        to_db(*device),
        to_db(*admin),
        version,
        sealed,
      )
      .execute(&mut *tx)
      .await?;
    }
    tx.commit().await?;
    Ok(ShareOutcome::Stored)
  }

  /// The newest key version for which shares exist (0: none).
  pub async fn org_recovery_key_version(&self) -> Result<u64, DbError> {
    let version =
      sqlx::query_scalar!(r#"SELECT COALESCE(max(key_version), 0) AS "version!" FROM org_recovery_shares"#)
        .fetch_one(self.pool())
        .await?;
    Ok(u64::try_from(version).unwrap_or(0))
  }

  pub async fn list_org_recovery_shares_of(&self, admin: UserId) -> Result<Vec<StoredShare>, DbError> {
    let rows = sqlx::query!(
      "SELECT device_id, admin_id, key_version, sealed FROM org_recovery_shares WHERE admin_id = $1 ORDER BY device_id",
      to_db(admin),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredShare {
          device: from_db(row.device_id)?,
          admin: from_db(row.admin_id)?,
          key_version: u64::try_from(row.key_version).unwrap_or(0),
          sealed: row.sealed,
        })
      })
      .collect()
  }

  /// Active devices among `devices` whose owners are active admins or owners, with the owner.
  pub async fn admin_devices(&self, devices: &[DeviceId]) -> Result<Vec<(DeviceId, UserId)>, DbError> {
    let raw: Vec<i64> = devices.iter().map(|device| to_db(*device)).collect();
    let rows = sqlx::query!(
      r#"SELECT d.id, d.user_id FROM devices d JOIN users u ON u.id = d.user_id
         WHERE d.id = ANY($1) AND d.revoked_at IS NULL AND u.deactivated_at IS NULL
           AND u.oidc_role IN ('admin', 'owner')"#,
      &raw,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.id)?, from_db(row.user_id)?)))
      .collect()
  }

  /// Active admins and owners.
  pub async fn list_active_admin_ids(&self) -> Result<Vec<UserId>, DbError> {
    let rows =
      sqlx::query!("SELECT id FROM users WHERE oidc_role IN ('admin', 'owner') AND deactivated_at IS NULL")
        .fetch_all(self.pool())
        .await?;
    rows.into_iter().map(|row| from_db(row.id)).collect()
  }

  /// Marks open requests past their expiry.
  pub async fn expire_recovery_requests(&self, now: OffsetDateTime) -> Result<u64, DbError> {
    let result = sqlx::query!(
      "UPDATE org_recovery_requests SET status = 'expired' WHERE status IN ('pending', 'approved') AND expires_at <= $1",
      now,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected())
  }

  /// `false` if the person has an open request already.
  pub async fn create_recovery_request(&self, new: &NewRecoveryRequest<'_>) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"INSERT INTO org_recovery_requests
           (id, user_id, request_public_key, fingerprint, required_approvals, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING"#,
      to_db(new.id),
      to_db(new.user),
      new.request_public_key,
      new.fingerprint,
      i32::try_from(new.required_approvals).unwrap_or(i32::MAX),
      new.expires_at,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  pub async fn get_recovery_request(
    &self,
    id: RecoveryRequestId,
  ) -> Result<Option<StoredRecoveryRequest>, DbError> {
    let mut found = self.recovery_requests(Some(id), false).await?;
    Ok(found.pop())
  }

  /// Requests waiting for approvals or for the combining admin, oldest first.
  pub async fn list_open_recovery_requests(&self) -> Result<Vec<StoredRecoveryRequest>, DbError> {
    self.recovery_requests(None, true).await
  }

  /// The open request of the person, if any.
  pub async fn open_recovery_request_of(
    &self,
    user: UserId,
  ) -> Result<Option<StoredRecoveryRequest>, DbError> {
    Ok(
      self
        .recovery_requests(None, true)
        .await?
        .into_iter()
        .find(|request| request.user == user),
    )
  }

  async fn recovery_requests(
    &self,
    maybe_id: Option<RecoveryRequestId>,
    only_open: bool,
  ) -> Result<Vec<StoredRecoveryRequest>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, user_id, request_public_key, fingerprint, status, required_approvals, created_at, expires_at,
                combining_admin_id, combining_device_id, result_sealed IS NOT NULL AS "has_result!",
                (SELECT count(*) FROM org_recovery_request_shares s WHERE s.request_id = r.id) AS "shares_received!"
         FROM org_recovery_requests r
         WHERE ($1::bigint IS NULL OR r.id = $1)
           AND (NOT $2 OR r.status IN ('pending', 'approved'))
         ORDER BY r.id"#,
      maybe_id.map(to_db),
      only_open,
    )
    .fetch_all(self.pool())
    .await?;
    let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
    let approvals = sqlx::query!(
      "SELECT request_id, admin_id, device_id, approved_at FROM org_recovery_approvals
       WHERE request_id = ANY($1) ORDER BY approved_at, admin_id",
      &ids,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        let mut list = Vec::new();
        for approval in approvals.iter().filter(|a| a.request_id == row.id) {
          list.push(StoredApproval {
            admin: from_db(approval.admin_id)?,
            device: from_db(approval.device_id)?,
            approved_at: approval.approved_at,
          });
        }
        Ok(StoredRecoveryRequest {
          id: from_db(row.id)?,
          user: from_db(row.user_id)?,
          request_public_key: row.request_public_key,
          fingerprint: row.fingerprint,
          status: row.status,
          required_approvals: u32::try_from(row.required_approvals).unwrap_or(u32::MAX),
          created_at: row.created_at,
          expires_at: row.expires_at,
          maybe_combining_admin: from_db_opt(row.combining_admin_id)?,
          maybe_combining_device: from_db_opt(row.combining_device_id)?,
          has_result: row.has_result,
          approvals: list,
          shares_received: u32::try_from(row.shares_received).unwrap_or(u32::MAX),
        })
      })
      .collect()
  }

  /// Records an approval; the one that reaches the threshold makes its device the combining one.
  pub async fn approve_recovery_request(
    &self,
    id: RecoveryRequestId,
    admin: UserId,
    device: DeviceId,
  ) -> Result<ApprovalOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    let Some(request) = sqlx::query!(
      "SELECT status, required_approvals FROM org_recovery_requests WHERE id = $1 FOR UPDATE",
      to_db(id)
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
      return Ok(ApprovalOutcome::NotPending);
    };
    if request.status != "pending" {
      return Ok(ApprovalOutcome::NotPending);
    }
    let inserted = sqlx::query!(
      "INSERT INTO org_recovery_approvals (request_id, admin_id, device_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
      to_db(id),
      to_db(admin),
      to_db(device),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
      return Ok(ApprovalOutcome::AlreadyApproved);
    }
    let count = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM org_recovery_approvals WHERE request_id = $1"#,
      to_db(id)
    )
    .fetch_one(&mut *tx)
    .await?;
    let reached = count >= i64::from(request.required_approvals);
    if reached {
      sqlx::query!(
        "UPDATE org_recovery_requests SET status = 'approved', combining_admin_id = $2, combining_device_id = $3 WHERE id = $1",
        to_db(id),
        to_db(admin),
        to_db(device),
      )
      .execute(&mut *tx)
      .await?;
    }
    tx.commit().await?;
    Ok(if reached {
      ApprovalOutcome::ThresholdReached
    } else {
      ApprovalOutcome::Recorded
    })
  }

  /// Moves an open request to `status` (`denied`, `cancelled`). `false` if it was not open.
  pub async fn close_recovery_request(&self, id: RecoveryRequestId, status: &str) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE org_recovery_requests SET status = $2 WHERE id = $1 AND status IN ('pending', 'approved')",
      to_db(id),
      status,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  pub async fn put_recovery_request_share(
    &self,
    id: RecoveryRequestId,
    from_admin: UserId,
    sealed: &[u8],
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO org_recovery_request_shares (request_id, from_admin_id, sealed) VALUES ($1, $2, $3)
         ON CONFLICT (request_id, from_admin_id) DO UPDATE SET sealed = $3"#,
      to_db(id),
      to_db(from_admin),
      sealed,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn list_recovery_request_shares(
    &self,
    id: RecoveryRequestId,
  ) -> Result<Vec<(UserId, Vec<u8>)>, DbError> {
    let rows = sqlx::query!(
      "SELECT from_admin_id, sealed FROM org_recovery_request_shares WHERE request_id = $1 ORDER BY from_admin_id",
      to_db(id)
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.from_admin_id)?, row.sealed)))
      .collect()
  }

  /// Stores the sealed backup key and completes the request. `false` if it was not approved.
  pub async fn complete_recovery_request(
    &self,
    id: RecoveryRequestId,
    sealed: &[u8],
    now: OffsetDateTime,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE org_recovery_requests SET status = 'completed', result_sealed = $2, completed_at = $3
       WHERE id = $1 AND status = 'approved'",
      to_db(id),
      sealed,
      now,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  pub async fn get_recovery_result(&self, id: RecoveryRequestId) -> Result<Option<Vec<u8>>, DbError> {
    Ok(
      sqlx::query_scalar!(
        "SELECT result_sealed FROM org_recovery_requests WHERE id = $1",
        to_db(id)
      )
      .fetch_optional(self.pool())
      .await?
      .flatten(),
    )
  }
}
