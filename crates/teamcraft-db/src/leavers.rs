//! Leavers: accounts the identity provider deactivated that are still in conversations, spaces or
//! hold devices, and the claims of the apps that remove them (docs/provisioning.md §3).

use teamcraft_proto::{DeviceId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// A leaver as listed for the apps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLeaver {
  pub user_id: UserId,
  pub deactivated_at: OffsetDateTime,
  /// Another device's claim that hasn't run out.
  pub is_claimed_by_other: bool,
}

/// How a claim went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaverClaim {
  /// This device holds the claim until the lease runs out.
  Claimed,
  /// Another device holds a claim that hasn't run out.
  ClaimedByOther,
  /// Not a leaver (any more): active, deleted, deactivated otherwise, or nothing left to remove.
  NotALeaver,
}

impl Database {
  /// Leavers after `maybe_after`, ordered by ID, at most `limit`. `maybe_device` is the asking
  /// device: its own claims don't count as taken.
  pub async fn list_leavers(
    &self,
    maybe_after: Option<UserId>,
    maybe_device: Option<DeviceId>,
    limit: i64,
  ) -> Result<Vec<StoredLeaver>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT l.user_id AS "user_id!", l.deactivated_at AS "deactivated_at!",
                COALESCE(c.claimed_until > now() AND c.device_id IS DISTINCT FROM $2, false) AS "is_claimed!"
         FROM identity_provider_leavers l
         LEFT JOIN leaver_removal_claims c ON c.user_id = l.user_id
         WHERE l.user_id > $1
         ORDER BY l.user_id
         LIMIT $3"#,
      maybe_after.map_or(0, to_db),
      maybe_device.map(to_db) as Option<i64>,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredLeaver {
          user_id: from_db(row.user_id)?,
          deactivated_at: row.deactivated_at,
          is_claimed_by_other: row.is_claimed,
        })
      })
      .collect()
  }

  /// Claims `user` for `device` until `lease` from now, in one statement, so that of several devices
  /// (and server instances) exactly one holds it. The same device renews its own claim.
  pub async fn claim_leaver(
    &self,
    user: UserId,
    device: DeviceId,
    lease: time::Duration,
  ) -> Result<LeaverClaim, DbError> {
    let lease_secs = lease.as_seconds_f64();
    let claimed = sqlx::query_scalar!(
      r#"INSERT INTO leaver_removal_claims (user_id, device_id, claimed_until)
         SELECT l.user_id, $2, now() + make_interval(secs => $3)
         FROM identity_provider_leavers l WHERE l.user_id = $1
         ON CONFLICT (user_id) DO UPDATE
           SET device_id = EXCLUDED.device_id, claimed_until = EXCLUDED.claimed_until
           WHERE leaver_removal_claims.claimed_until <= now()
              OR leaver_removal_claims.device_id = EXCLUDED.device_id
         RETURNING user_id"#,
      to_db(user),
      to_db(device),
      lease_secs,
    )
    .fetch_optional(self.pool())
    .await?;
    if claimed.is_some() {
      return Ok(LeaverClaim::Claimed);
    }
    let is_held = sqlx::query_scalar!(
      r#"SELECT EXISTS (SELECT 1 FROM leaver_removal_claims
                        WHERE user_id = $1 AND claimed_until > now()) AS "held!""#,
      to_db(user),
    )
    .fetch_one(self.pool())
    .await?;
    // A held claim on somebody who is no leaver any more (reactivated, removed) is stale: say so.
    let is_leaver = self.is_leaver(user).await?;
    Ok(match (is_leaver, is_held) {
      (true, true) => LeaverClaim::ClaimedByOther,
      _ => LeaverClaim::NotALeaver,
    })
  }

  /// Forgets the claim once its work is done or the account is active again.
  pub async fn release_leaver_claim(&self, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM leaver_removal_claims WHERE user_id = $1",
      to_db(user)
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  async fn is_leaver(&self, user: UserId) -> Result<bool, DbError> {
    let is_leaver = sqlx::query_scalar!(
      r#"SELECT EXISTS (SELECT 1 FROM identity_provider_leavers WHERE user_id = $1) AS "leaver!""#,
      to_db(user),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(is_leaver)
  }
}
