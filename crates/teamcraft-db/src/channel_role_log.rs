//! Each channel's signed role log (issue #380, e2ee.md §4.6.1). Payload and signature are opaque
//! bytes here; clients verify them.

use teamcraft_proto::{ConversationId, DeviceId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db_opt, to_db};
use crate::workspace_log::AppendOutcome;

/// One entry of a channel's role log.
#[derive(Debug, Clone)]
pub struct ChannelRoleLogEntry {
  pub version: u64,
  pub payload: Vec<u8>,
  pub signature: Vec<u8>,
  pub maybe_signer: Option<DeviceId>,
  pub created_at: OffsetDateTime,
}

impl Database {
  /// Appends `expected_version + 1` to the channel's role log if it is still at `expected_version`.
  /// Concurrent appends of the same version race on the primary key and exactly one wins.
  pub async fn append_channel_role_log(
    &self,
    conversation: ConversationId,
    expected_version: u64,
    payload: &[u8],
    signature: &[u8],
    signer: DeviceId,
  ) -> Result<AppendOutcome, DbError> {
    let expected =
      i64::try_from(expected_version).map_err(|_| DbError::InvalidStoredValue("version".into()))?;
    let result = sqlx::query!(
      r#"INSERT INTO channel_role_log (conversation_id, version, payload, signature, signer_device_id)
         SELECT $1, $2::bigint + 1, $3, $4, $5
         WHERE COALESCE((SELECT max(version) FROM channel_role_log WHERE conversation_id = $1), 0) = $2
         ON CONFLICT DO NOTHING"#,
      to_db(conversation),
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
    let current = self.channel_role_log_version(conversation).await?;
    Ok(AppendOutcome::VersionConflict {
      current_version: current,
    })
  }

  pub async fn channel_role_log_version(&self, conversation: ConversationId) -> Result<u64, DbError> {
    let version = sqlx::query_scalar!(
      r#"SELECT COALESCE(max(version), 0) AS "version!" FROM channel_role_log WHERE conversation_id = $1"#,
      to_db(conversation),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(u64::try_from(version).unwrap_or(0))
  }

  /// Entries after `after`, oldest first (`limit + 1` rows so callers can tell if more exist).
  pub async fn list_channel_role_log(
    &self,
    conversation: ConversationId,
    after: u64,
    limit: i64,
  ) -> Result<Vec<ChannelRoleLogEntry>, DbError> {
    let after = i64::try_from(after).unwrap_or(i64::MAX);
    let rows = sqlx::query!(
      r#"SELECT version, payload, signature, signer_device_id, created_at FROM channel_role_log
         WHERE conversation_id = $1 AND version > $2 ORDER BY version ASC LIMIT $3"#,
      to_db(conversation),
      after,
      limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ChannelRoleLogEntry {
          version: u64::try_from(row.version).unwrap_or(0),
          payload: row.payload,
          signature: row.signature,
          maybe_signer: from_db_opt(row.signer_device_id)?,
          created_at: row.created_at,
        })
      })
      .collect()
  }
}
