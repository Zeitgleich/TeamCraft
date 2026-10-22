use teamcraft_proto::{DeviceId, WorkspaceLogKind};
use time::OffsetDateTime;

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
}

impl Database {
  /// Appends `expected_version + 1` if the log is still at `expected_version`. Concurrent appends of
  /// the same version race on the primary key and exactly one wins.
  pub async fn append_workspace_log(
    &self,
    kind: &WorkspaceLogKind,
    expected_version: u64,
    payload: &[u8],
    signature: &[u8],
    signer: DeviceId,
  ) -> Result<AppendOutcome, DbError> {
    let expected =
      i64::try_from(expected_version).map_err(|_| DbError::InvalidStoredValue("version".into()))?;
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
    .execute(self.pool())
    .await?;
    if result.rows_affected() == 1 {
      return Ok(AppendOutcome::Appended);
    }
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
}
