use sqlx::{Postgres, Transaction};
use teamcraft_proto::{ConversationId, DeviceId, WorkspaceLogKind};
use time::OffsetDateTime;

use crate::audit::NewAuditEntry;
use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db_opt, to_db};
use crate::legal_hold::{HeldPerson, HoldsPlaced};

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

/// An entry to append to the roster or policy log.
#[derive(Debug, Clone, Copy)]
pub struct NewWorkspaceLogEntry<'a> {
  pub kind: &'a WorkspaceLogKind,
  /// The version the signer saw; the entry becomes the next one, or conflicts.
  pub expected_version: u64,
  pub payload: &'a [u8],
  pub signature: &'a [u8],
  pub signer: DeviceId,
  /// The conversations a policy entry places a legal hold on that the previous entry didn't hold;
  /// each must be an existing channel, direct message or group.
  pub new_holds: &'a [ConversationId],
  /// The people a policy entry holds, all of them (issue #2135); `None` for a roster entry. Each must
  /// be a known account.
  pub maybe_held_people: Option<&'a [HeldPerson]>,
  /// Audit rows written with the entry, only when it is.
  pub audit: &'a [NewAuditEntry<'a>],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
  Appended,
  /// The log is not at `expected_version` (anymore).
  VersionConflict {
    current_version: u64,
  },
  /// A new legal hold names a conversation that is not an existing channel, direct message or group
  /// (anymore): nothing is stored (data-export.md §4.2).
  NotExistingConversations,
  /// A person hold names an unknown account: nothing is stored.
  UnknownPeople,
}

impl Database {
  /// Appends `expected_version + 1` if the log is still at `expected_version`. Concurrent appends of
  /// the same version race on the primary key and exactly one wins. `audit` rows are written in the
  /// same transaction, only when the entry is.
  ///
  /// The conversations a policy entry newly holds ([`NewWorkspaceLogEntry::new_holds`]), and those
  /// of the people it newly holds, are share-locked and checked in this transaction, before the
  /// entry is written, so a hold and a deletion of the same conversation can't both win (issue
  /// #2143): a deletion or retention locks the conversation's row and reads the holds only after it
  /// has that lock ([`Self::hold_evidence_in`]). Whichever runs second sees the other's result: the
  /// hold is refused for a deleted conversation (a person's hold just doesn't cover it), or the
  /// deletion reads the holds. The held people are written in the same transaction.
  pub async fn append_workspace_log(
    &self,
    entry: &NewWorkspaceLogEntry<'_>,
  ) -> Result<AppendOutcome, DbError> {
    let NewWorkspaceLogEntry {
      kind,
      expected_version,
      payload,
      signature,
      signer,
      new_holds,
      maybe_held_people,
      audit,
    } = *entry;
    let expected =
      i64::try_from(expected_version).map_err(|_| DbError::InvalidStoredValue("version".into()))?;
    let mut tx = self.pool().begin().await?;
    match Self::place_holds_in(&mut tx, new_holds, maybe_held_people).await? {
      HoldsPlaced::Placed => {},
      HoldsPlaced::NotExistingConversations => {
        tx.rollback().await?;
        return Ok(AppendOutcome::NotExistingConversations);
      },
      HoldsPlaced::UnknownPeople => {
        tx.rollback().await?;
        return Ok(AppendOutcome::UnknownPeople);
      },
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
