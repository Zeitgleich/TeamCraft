//! Idempotency keys of create requests (space, channel, meeting).

use sqlx::{Postgres, Transaction};
use teamcraft_proto::{IdempotencyKey, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::to_db;

/// Keys older than this can no longer be retried by any client and are purged.
const KEY_RETENTION_DAYS: i32 = 7;
/// Rows purged per run, so the purge never holds locks for long.
const PURGE_BATCH: i64 = 1000;

/// What a create call made: the resource and whether this call created it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Created<T> {
  pub id: T,
  /// `false`: the idempotency key was already used and `id` is the original resource.
  pub is_new: bool,
}

/// The kind of resource a key was used for; keys of different kinds never collide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IdempotentKind {
  Space,
  Channel,
  Meeting,
}

impl IdempotentKind {
  pub(crate) fn as_str(self) -> &'static str {
    match self {
      Self::Space => "space",
      Self::Channel => "channel",
      Self::Meeting => "meeting",
    }
  }
}

/// Reserves the key for `resource` inside the creating transaction. Returns the resource ID stored
/// by an earlier request if the key was already used; the caller then returns it without creating
/// anything. A concurrent request with the same key waits for the first one's commit. One statement
/// does both steps, so a key removed in between can't leave the caller without an answer.
pub(crate) async fn claim_key(
  tx: &mut Transaction<'_, Postgres>,
  user: UserId,
  kind: IdempotentKind,
  key: &IdempotencyKey,
  resource: i64,
) -> Result<Option<i64>, DbError> {
  let row = sqlx::query!(
    r#"INSERT INTO create_idempotency_keys (user_id, kind, idempotency_key, resource_id)
       VALUES ($1, $2, $3, $4)
       ON CONFLICT (user_id, kind, idempotency_key)
       DO UPDATE SET resource_id = create_idempotency_keys.resource_id
       RETURNING resource_id, (xmax = 0) AS "is_inserted!""#,
    to_db(user),
    kind.as_str(),
    key.as_bytes().as_slice(),
    resource,
  )
  .fetch_one(&mut **tx)
  .await?;
  Ok((!row.is_inserted).then_some(row.resource_id))
}

impl Database {
  /// Purges one batch of keys past the retry window (run periodically, never in a request).
  /// Returns how many were removed; a full batch means there may be more.
  pub async fn delete_expired_create_keys(&self) -> Result<u64, DbError> {
    let result = sqlx::query!(
      r#"DELETE FROM create_idempotency_keys WHERE ctid IN (
           SELECT ctid FROM create_idempotency_keys
           WHERE created_at < now() - make_interval(days => $1) LIMIT $2 FOR UPDATE SKIP LOCKED)"#,
      KEY_RETENTION_DAYS,
      PURGE_BATCH,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected())
  }
}
