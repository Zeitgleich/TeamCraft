//! Idempotency keys of create requests (space, channel, meeting).

use sqlx::{Postgres, Transaction};
use teamcraft_proto::{IdempotencyKey, UserId};

use crate::error::DbError;
use crate::ids::to_db;

/// Keys older than this can no longer be retried by any client and are purged.
const KEY_RETENTION_DAYS: i32 = 7;

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
/// anything. A concurrent request with the same key waits for the first one's commit.
pub(crate) async fn claim_key(
  tx: &mut Transaction<'_, Postgres>,
  user: UserId,
  kind: IdempotentKind,
  key: &IdempotencyKey,
  resource: i64,
) -> Result<Option<i64>, DbError> {
  sqlx::query!(
    "DELETE FROM create_idempotency_keys WHERE created_at < now() - make_interval(days => $1)",
    KEY_RETENTION_DAYS,
  )
  .execute(&mut **tx)
  .await?;
  let inserted = sqlx::query_scalar!(
    r#"INSERT INTO create_idempotency_keys (user_id, kind, idempotency_key, resource_id)
       VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING resource_id"#,
    to_db(user),
    kind.as_str(),
    key.as_bytes().as_slice(),
    resource,
  )
  .fetch_optional(&mut **tx)
  .await?;
  if inserted.is_some() {
    return Ok(None);
  }
  let existing = sqlx::query_scalar!(
    "SELECT resource_id FROM create_idempotency_keys WHERE user_id = $1 AND kind = $2 AND idempotency_key = $3",
    to_db(user),
    kind.as_str(),
    key.as_bytes().as_slice(),
  )
  .fetch_one(&mut **tx)
  .await?;
  Ok(Some(existing))
}
