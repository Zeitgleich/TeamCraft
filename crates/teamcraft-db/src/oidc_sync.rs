//! The state of the identity provider sync (ARCHITECTURE.md §5.1.1): how the last run went, and the
//! deactivations its safety brake holds back.

use teamcraft_proto::UserId;
use time::OffsetDateTime;

use crate::audit::NewAuditEntry;
use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// One finished run, as stored.
#[derive(Debug, Clone)]
pub struct OidcSyncRun<'a> {
  pub started_at: OffsetDateTime,
  pub finished_at: OffsetDateTime,
  pub duration_ms: i64,
  pub outcome: &'a str,
  /// A fixed error code, never the error's text.
  pub maybe_error: Option<&'a str>,
  pub is_success: bool,
  pub deactivated: u32,
  pub reactivated: u32,
  pub role_changes: u32,
  pub groups_changed: u32,
  pub failed: u32,
  /// Accounts made before their first sign-in (docs/provisioning.md §6).
  pub created: u32,
  /// Accounts that never signed in, removed outright.
  pub deleted: u32,
}

/// The stored status of the sync.
#[derive(Debug, Clone, Default)]
pub struct OidcSyncStatusRow {
  pub maybe_started_at: Option<OffsetDateTime>,
  pub maybe_finished_at: Option<OffsetDateTime>,
  pub maybe_last_success_at: Option<OffsetDateTime>,
  pub maybe_duration_ms: Option<i64>,
  pub maybe_outcome: Option<String>,
  pub maybe_error: Option<String>,
  pub consecutive_failures: u32,
  pub deactivated: u32,
  pub reactivated: u32,
  pub role_changes: u32,
  pub groups_changed: u32,
  pub failed: u32,
  pub created: u32,
  pub deleted: u32,
  pub held_revision: i64,
}

/// A deactivation the safety brake holds back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeldDeactivation {
  pub user: UserId,
  pub cause: String,
  /// `oidc_sync` or `scim` (`DeactivationSource`).
  pub source: String,
  pub held_at: OffsetDateTime,
  pub is_confirmed: bool,
  pub display_name: String,
  pub maybe_username: Option<String>,
  pub maybe_oidc_role: Option<String>,
}

/// What a run wants held from now on.
#[derive(Debug, Clone)]
pub struct NewHeldDeactivation<'a> {
  pub user: UserId,
  pub cause: &'a str,
}

/// How a confirmation of the held deactivations went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrakeConfirmation {
  /// This many held deactivations are confirmed now.
  Confirmed(u32),
  /// The set changed since the admin looked at it.
  StaleRevision,
}

impl Database {
  /// Stores how a run went. A failure counts up, anything else starts the count again.
  pub async fn record_oidc_sync_run(&self, run: &OidcSyncRun<'_>) -> Result<(), DbError> {
    let as_i32 = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
    sqlx::query!(
      r#"INSERT INTO oidc_sync_status (id, last_started_at, last_finished_at, last_success_at, last_duration_ms,
                                       last_outcome, last_error, consecutive_failures, deactivated, reactivated,
                                       role_changes, groups_changed, failed, created, deleted)
         VALUES (true, $1, $2::timestamptz, CASE WHEN $3::boolean THEN $2::timestamptz END, $4, $5, $6, CASE WHEN $3::boolean THEN 0 ELSE 1 END, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (id) DO UPDATE SET
           last_started_at = EXCLUDED.last_started_at,
           last_finished_at = EXCLUDED.last_finished_at,
           last_success_at = COALESCE(EXCLUDED.last_success_at, oidc_sync_status.last_success_at),
           last_duration_ms = EXCLUDED.last_duration_ms,
           last_outcome = EXCLUDED.last_outcome,
           last_error = EXCLUDED.last_error,
           consecutive_failures = CASE WHEN $3::boolean THEN 0 ELSE oidc_sync_status.consecutive_failures + 1 END,
           deactivated = EXCLUDED.deactivated,
           reactivated = EXCLUDED.reactivated,
           role_changes = EXCLUDED.role_changes,
           groups_changed = EXCLUDED.groups_changed,
           failed = EXCLUDED.failed,
           created = EXCLUDED.created,
           deleted = EXCLUDED.deleted"#,
      run.started_at,
      run.finished_at,
      run.is_success,
      run.duration_ms,
      run.outcome,
      run.maybe_error,
      as_i32(run.deactivated),
      as_i32(run.reactivated),
      as_i32(run.role_changes),
      as_i32(run.groups_changed),
      as_i32(run.failed),
      as_i32(run.created),
      as_i32(run.deleted),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// How the sync stands; defaults before its first run.
  pub async fn oidc_sync_status(&self) -> Result<OidcSyncStatusRow, DbError> {
    let maybe_row = sqlx::query!(
      r#"SELECT last_started_at, last_finished_at, last_success_at, last_duration_ms, last_outcome, last_error,
                consecutive_failures, deactivated, reactivated, role_changes, groups_changed, failed, created, deleted,
                held_revision
         FROM oidc_sync_status"#
    )
    .fetch_optional(self.pool())
    .await?;
    let Some(row) = maybe_row else {
      return Ok(OidcSyncStatusRow::default());
    };
    let as_u32 = |value: i32| u32::try_from(value).unwrap_or(0);
    Ok(OidcSyncStatusRow {
      maybe_started_at: row.last_started_at,
      maybe_finished_at: row.last_finished_at,
      maybe_last_success_at: row.last_success_at,
      maybe_duration_ms: row.last_duration_ms,
      maybe_outcome: row.last_outcome,
      maybe_error: row.last_error,
      consecutive_failures: as_u32(row.consecutive_failures),
      deactivated: as_u32(row.deactivated),
      reactivated: as_u32(row.reactivated),
      role_changes: as_u32(row.role_changes),
      groups_changed: as_u32(row.groups_changed),
      failed: as_u32(row.failed),
      created: as_u32(row.created),
      deleted: as_u32(row.deleted),
      held_revision: row.held_revision,
    })
  }

  /// The deactivations held back, oldest first, with who they are about.
  pub async fn list_held_deactivations(&self) -> Result<Vec<HeldDeactivation>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT h.user_id, h.cause, h.source, h.held_at, h.confirmed_at IS NOT NULL AS "is_confirmed!",
                u.display_name, u.username, u.oidc_role
         FROM oidc_sync_held_deactivations h JOIN users u ON u.id = h.user_id
         ORDER BY h.held_at, h.user_id"#
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(HeldDeactivation {
          user: from_db(row.user_id)?,
          cause: row.cause,
          source: row.source,
          held_at: row.held_at,
          is_confirmed: row.is_confirmed,
          display_name: row.display_name,
          maybe_username: row.username,
          maybe_oidc_role: row.oidc_role,
        })
      })
      .collect()
  }

  /// Replaces what is held by `held`: others go, new ones are added, ones that stay keep their
  /// confirmation. The revision moves if anything changed, so an older confirmation is refused.
  /// Returns how many are held now.
  pub async fn set_held_deactivations(&self, held: &[NewHeldDeactivation<'_>]) -> Result<u32, DbError> {
    let mut tx = self.pool().begin().await?;
    let ids: Vec<i64> = held.iter().map(|entry| to_db(entry.user)).collect();
    let causes: Vec<&str> = held.iter().map(|entry| entry.cause).collect();
    let removed = sqlx::query!(
      "DELETE FROM oidc_sync_held_deactivations WHERE source = 'oidc_sync' AND NOT (user_id = ANY($1))",
      &ids,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let added = sqlx::query!(
      r#"INSERT INTO oidc_sync_held_deactivations (user_id, cause)
         SELECT * FROM UNNEST($1::bigint[], $2::text[])
         ON CONFLICT (user_id) DO NOTHING"#,
      &ids,
      &causes as &[&str],
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if removed + added > 0 {
      bump_held_revision(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(u32::try_from(ids.len()).unwrap_or(u32::MAX))
  }

  /// Confirms every held deactivation, if the set is still the one with `revision`, and records
  /// who did in the same transaction.
  pub async fn confirm_held_deactivations(
    &self,
    actor: UserId,
    revision: i64,
    audit: &NewAuditEntry<'_>,
  ) -> Result<BrakeConfirmation, DbError> {
    let mut tx = self.pool().begin().await?;
    let current = sqlx::query_scalar!("SELECT held_revision FROM oidc_sync_status FOR UPDATE")
      .fetch_optional(&mut *tx)
      .await?;
    if current != Some(revision) {
      return Ok(BrakeConfirmation::StaleRevision);
    }
    let confirmed = sqlx::query!(
      r#"UPDATE oidc_sync_held_deactivations SET confirmed_at = now(), confirmed_by = $1
         WHERE confirmed_at IS NULL"#,
      to_db(actor),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if confirmed > 0 {
      bump_held_revision(&mut tx).await?;
      sqlx::query!(
        r#"INSERT INTO audit_log (id, actor_user_id, action, target_kind, target_id, detail)
           VALUES ($1, $2, $3, $4, $5, ($6::text::jsonb || jsonb_build_object('confirmed', $7::bigint)))"#,
        audit.id.get(),
        audit.maybe_actor.map(to_db),
        audit.action,
        audit.maybe_target_kind,
        audit.maybe_target_id,
        audit.detail,
        i64::try_from(confirmed).unwrap_or(i64::MAX),
      )
      .execute(&mut *tx)
      .await?;
    }
    tx.commit().await?;
    Ok(BrakeConfirmation::Confirmed(
      u32::try_from(confirmed).unwrap_or(u32::MAX),
    ))
  }

  /// Deactivated accounts that still have a device or a place in a conversation or space: nobody
  /// has removed them yet (admin.md §3.2.1), so their devices stay in the encryption groups.
  pub async fn count_pending_chat_removals(&self) -> Result<u32, DbError> {
    let count = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM users u
         WHERE u.deactivated_at IS NOT NULL AND u.deleted_at IS NULL AND u.kind <> 'bot'
           AND (EXISTS (SELECT 1 FROM memberships m WHERE m.user_id = u.id)
             OR EXISTS (SELECT 1 FROM space_members m WHERE m.user_id = u.id)
             OR EXISTS (SELECT 1 FROM devices d WHERE d.user_id = u.id AND d.revoked_at IS NULL))"#
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
  }

  /// Who is deactivated with follow-up work left, oldest first.
  pub async fn pending_deactivation_followups(&self, limit: i64) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      "SELECT user_id FROM user_deactivation_followups ORDER BY created_at, user_id LIMIT $1",
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// How many deactivations still have work left.
  pub async fn count_deactivation_followups(&self) -> Result<u32, DbError> {
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM user_deactivation_followups"#)
      .fetch_one(self.pool())
      .await?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
  }

  /// What the follow-up of a deactivation works from. `None` if it was lifted meanwhile.
  pub async fn deactivation_followup(&self, user: UserId) -> Result<Option<crate::Deactivation>, DbError> {
    let is_deactivated = sqlx::query_scalar!(
      r#"SELECT deactivated_at IS NOT NULL AS "is_deactivated!" FROM users WHERE id = $1"#,
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    if is_deactivated != Some(true) {
      return Ok(None);
    }
    let mut connection = self.pool().acquire().await?;
    let (conversations, co_members) = crate::admin::deactivation_audience(&mut connection, user).await?;
    Ok(Some(crate::Deactivation {
      was_already_deactivated: true,
      conversations,
      co_members,
      paused_bots: Vec::new(),
    }))
  }

  /// Notes that the follow-up was tried, so an operator can tell a stuck one.
  pub async fn note_deactivation_followup_attempt(&self, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE user_deactivation_followups SET attempts = attempts + 1, last_attempt_at = now() WHERE user_id = $1",
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// The follow-up of a deactivation is done.
  pub async fn finish_deactivation_followup(&self, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM user_deactivation_followups WHERE user_id = $1",
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }
}

pub(crate) async fn bump_held_revision(
  tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), DbError> {
  sqlx::query!(
    r#"INSERT INTO oidc_sync_status (id, held_revision) VALUES (true, 1)
       ON CONFLICT (id) DO UPDATE SET held_revision = oidc_sync_status.held_revision + 1"#
  )
  .execute(&mut **tx)
  .await?;
  Ok(())
}
