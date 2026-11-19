use sqlx::{Postgres, Transaction};
use teamcraft_proto::{ConversationId, DeviceId, WorkspaceLogKind};
use time::OffsetDateTime;

use crate::audit::NewAuditEntry;
use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db_opt, to_db};

/// One entry of the signed roster or policy log. Payload and signature are opaque bytes here.
#[derive(Debug, Clone)]
pub struct WorkspaceLogEntry {
  pub kind: WorkspaceLogKind,
  pub version: u64,
  pub payload: Vec<u8>,
  pub signature: Vec<u8>,
  pub maybe_signer: Option<DeviceId>,
  pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
  Appended,
  /// The log is not at `expected_version` (anymore).
  VersionConflict {
    current_version: u64,
  },
  /// A new legal hold names a conversation that is not an existing channel (anymore): nothing is
  /// stored (data-export.md §4.2).
  NotExistingChannels,
}

impl Database {
  /// Appends `expected_version + 1` if the log is still at `expected_version`. Concurrent appends of
  /// the same version race on the primary key and exactly one wins. `audit` rows are written in the
  /// same transaction, only when the entry is.
  ///
  /// `new_holds` are the channels a policy entry places a legal hold on. They are share-locked and
  /// checked in this transaction, before the entry is written, so a hold and a deletion of the same
  /// channel can't both win (issue #2143): a deletion holds the channel's row for update and reads the
  /// newest policy only after it has that lock ([`Self::latest_policy_payload_in`]). Whichever runs
  /// second sees the other's result: the hold is refused for a deleted channel, or the deletion reads
  /// the policy that holds it.
  #[allow(clippy::too_many_arguments)]
  pub async fn append_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    expected_version: u64,
    payload: &[u8],
    signature: &[u8],
    signer: DeviceId,
    new_holds: &[ConversationId],
    audit: &[NewAuditEntry<'_>],
  ) -> Result<AppendOutcome, DbError> {
    let expected =
      i64::try_from(expected_version).map_err(|_| DbError::InvalidStoredValue("version".into()))?;
    let mut tx = self.pool().begin().await?;
    if !lock_existing_channels(&mut tx, new_holds).await? {
      tx.rollback().await?;
      return Ok(AppendOutcome::NotExistingChannels);
    }
    let result = sqlx::query!(
      r#"INSERT INTO workspace_log (kind, version, payload, signature, signer_device_id)
         SELECT $1, $2::bigint + 1, $3, $4, $5
         WHERE COALESCE((SELECT max(version) FROM workspace_log WHERE kind = $1), 0) = $2
         ON CONFLICT DO NOTHING"#,
      kind.as_str(),
      expected,
      payload,
      signature,
      to_db(signer),
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 1 {
      // The entry and what it is audited as go together (data-export.md §4.2).
      for entry in audit {
        Self::record_audit_in(&mut tx, entry).await?;
      }
      tx.commit().await?;
      return Ok(AppendOutcome::Appended);
    }
    tx.rollback().await?;
    let current = self.workspace_log_version(kind).await?;
    Ok(AppendOutcome::VersionConflict {
      current_version: current,
    })
  }

  pub async fn workspace_log_version(&self, kind: &WorkspaceLogKind) -> Result<u64, DbError> {
    let version = sqlx::query_scalar!(
      r#"SELECT COALESCE(max(version), 0) AS "version!" FROM workspace_log WHERE kind = $1"#,
      kind.as_str(),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u64::try_from(version).unwrap_or(0))
  }

  /// Entries after `after`, oldest first (`limit + 1` rows so callers can tell if more exist).
  pub async fn list_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    after: u64,
    limit: i64,
  ) -> Result<Vec<WorkspaceLogEntry>, DbError> {
    let after = i64::try_from(after).unwrap_or(i64::MAX);
    let rows = sqlx::query!(
      r#"SELECT version, payload, signature, signer_device_id, created_at FROM workspace_log
         WHERE kind = $1 AND version > $2 ORDER BY version ASC LIMIT $3"#,
      kind.as_str(),
      after,
      limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(WorkspaceLogEntry {
          kind: kind.clone(),
          version: u64::try_from(row.version).unwrap_or(0),
          payload: row.payload,
          signature: row.signature,
          maybe_signer: from_db_opt(row.signer_device_id)?,
          created_at: row.created_at,
        })
      })
      .collect()
  }

  pub async fn latest_workspace_log_entry(
    &self,
    kind: &WorkspaceLogKind,
  ) -> Result<Option<WorkspaceLogEntry>, DbError> {
    let row = sqlx::query!(
      r#"SELECT version, payload, signature, signer_device_id, created_at FROM workspace_log
         WHERE kind = $1 ORDER BY version DESC LIMIT 1"#,
      kind.as_str(),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(WorkspaceLogEntry {
          kind: kind.clone(),
          version: u64::try_from(row.version).unwrap_or(0),
          payload: row.payload,
          signature: row.signature,
          maybe_signer: from_db_opt(row.signer_device_id)?,
          created_at: row.created_at,
        })
      })
      .transpose()
  }

  /// The newest policy entry's payload as stored, read in a deletion's transaction after it locked
  /// the rows it deletes: a hold whose entry committed before those locks were granted is in it, and
  /// one appended later waits for the deletion and then finds the channel gone
  /// ([`Self::append_workspace_log`], issue #2143).
  pub(crate) async fn latest_policy_payload_in(
    tx: &mut Transaction<'_, Postgres>,
  ) -> Result<Option<Vec<u8>>, DbError> {
    let payload = sqlx::query_scalar!(
      r#"SELECT payload FROM workspace_log WHERE kind = 'policy' ORDER BY version DESC LIMIT 1"#,
    )
    .fetch_optional(&mut **tx)
    .await?;
    Ok(payload)
  }
}

/// Share-locks `channels` if every one of them is an existing channel, in id order like the deletion
/// of a space locks them. `false` when one is missing, deleted or no channel. A deletion that holds
/// one of them makes this wait, and its result is then judged on the row as the deletion left it.
async fn lock_existing_channels(
  tx: &mut Transaction<'_, Postgres>,
  channels: &[ConversationId],
) -> Result<bool, DbError> {
  if channels.is_empty() {
    return Ok(true);
  }
  let mut ids: Vec<i64> = channels.iter().copied().map(to_db).collect();
  ids.sort_unstable();
  ids.dedup();
  let locked = sqlx::query_scalar!(
    r#"SELECT id FROM conversations
       WHERE id = ANY($1) AND kind = 'channel' AND deleted_at IS NULL
       ORDER BY id FOR SHARE"#,
    &ids,
  )
  .fetch_all(&mut **tx)
  .await?;
  Ok(locked.len() == ids.len())
}
