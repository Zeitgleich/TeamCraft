//! Call recordings that run, for the audit log (issue #2117): the SFU reports when a device's
//! recording notice starts or stops a recording; the start and the stop each write one audit row,
//! in the same transaction as the row here that ties them together.

use teamcraft_proto::{CallId, ConversationId, DeviceId, Snowflake, UserId};

use crate::audit::NewAuditEntry;
use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// What the audit rows of recordings are about: the call's conversation.
const TARGET_KIND: &str = "conversation";

/// A recording the SFU reported as started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallRecordingStart {
  pub call_id: CallId,
  pub device_id: DeviceId,
  pub user_id: UserId,
  pub conversation_id: ConversationId,
  /// Devices in the call at the start.
  pub participants: u32,
}

/// A recording that ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndedCallRecording {
  pub call_id: CallId,
  pub device_id: DeviceId,
  pub user_id: UserId,
  pub conversation_id: ConversationId,
  /// From its start to now, by the database's clock; whole seconds, never negative.
  pub duration_secs: u64,
  /// The most devices in the call at once while it ran.
  pub peak_participants: u32,
}

/// A recording that runs, as far as the server knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunningCallRecording {
  pub call_id: CallId,
  pub device_id: DeviceId,
}

/// The audit row a start or stop writes: its id and action; the actor is the recorder, the target
/// the call's conversation.
#[derive(Debug, Clone, Copy)]
pub struct RecordingAudit<'a> {
  pub id: Snowflake,
  pub action: &'a str,
}

impl Database {
  /// Notes a recording that started, with its audit row (`detail`). Returns `false`, and writes no
  /// audit row, when it runs already: the SFU or the recorder restarted and announced it again, so
  /// it keeps its start and only its participant count can grow.
  pub async fn start_call_recording(
    &self,
    start: &CallRecordingStart,
    audit: RecordingAudit<'_>,
    detail: &str,
  ) -> Result<bool, DbError> {
    let participants = count_to_db(start.participants);
    let mut tx = self.pool().begin().await?;
    let is_new = sqlx::query_scalar!(
      r#"INSERT INTO open_call_recordings (call_id, device_id, user_id, conversation_id, peak_participants)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (call_id, device_id) DO UPDATE
           SET peak_participants = GREATEST(open_call_recordings.peak_participants, EXCLUDED.peak_participants)
         RETURNING (xmax = 0) AS "is_new!""#,
      to_db(start.call_id),
      to_db(start.device_id),
      to_db(start.user_id),
      to_db(start.conversation_id),
      participants,
    )
    .fetch_one(&mut *tx)
    .await?;
    if is_new {
      Self::record_audit_in(
        &mut tx,
        &NewAuditEntry {
          id: audit.id,
          maybe_actor: Some(start.user_id),
          action: audit.action,
          maybe_target_kind: Some(TARGET_KIND),
          maybe_target_id: Some(to_db(start.conversation_id)),
          detail,
        },
      )
      .await?;
    }
    tx.commit().await?;
    Ok(is_new)
  }

  /// Ends a running recording with its audit row, whose detail `detail` makes from what ended.
  /// `participants` is the reporter's count of the most devices at once (0 when it has none).
  /// `None`, and no audit row, when no such recording runs (its stop came twice, or it was never
  /// reported as started).
  pub async fn end_call_recording(
    &self,
    running: RunningCallRecording,
    participants: u32,
    audit: RecordingAudit<'_>,
    detail: impl FnOnce(&EndedCallRecording) -> String,
  ) -> Result<Option<EndedCallRecording>, DbError> {
    let mut tx = self.pool().begin().await?;
    let Some(row) = sqlx::query!(
      r#"DELETE FROM open_call_recordings
         WHERE call_id = $1 AND device_id = $2
         RETURNING user_id, conversation_id, peak_participants,
           GREATEST(0, EXTRACT(EPOCH FROM now() - started_at))::bigint AS "duration_secs!""#,
      to_db(running.call_id),
      to_db(running.device_id),
    )
    .fetch_optional(&mut *tx)
    .await?
    else {
      return Ok(None);
    };
    let ended = EndedCallRecording {
      call_id: running.call_id,
      device_id: running.device_id,
      user_id: from_db(row.user_id)?,
      conversation_id: from_db(row.conversation_id)?,
      duration_secs: u64::try_from(row.duration_secs).unwrap_or(0),
      peak_participants: u32::try_from(row.peak_participants)
        .unwrap_or(0)
        .max(participants),
    };
    let detail = detail(&ended);
    Self::record_audit_in(
      &mut tx,
      &NewAuditEntry {
        id: audit.id,
        maybe_actor: Some(ended.user_id),
        action: audit.action,
        maybe_target_kind: Some(TARGET_KIND),
        maybe_target_id: Some(to_db(ended.conversation_id)),
        detail: &detail,
      },
    )
    .await?;
    tx.commit().await?;
    Ok(Some(ended))
  }

  /// Every recording the server thinks runs, oldest first.
  pub async fn running_call_recordings(&self) -> Result<Vec<RunningCallRecording>, DbError> {
    let rows = sqlx::query!("SELECT call_id, device_id FROM open_call_recordings ORDER BY started_at")
      .fetch_all(self.pool())
      .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(RunningCallRecording {
          call_id: from_db(row.call_id)?,
          device_id: from_db(row.device_id)?,
        })
      })
      .collect()
  }
}

fn count_to_db(count: u32) -> i32 {
  i32::try_from(count).unwrap_or(i32::MAX)
}
