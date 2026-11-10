//! The identity provider's event hook (docs/provisioning.md §4): the hash of its secret and when it
//! was last used.

use teamcraft_proto::UserId;
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::to_db;

/// The stored hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEventHook {
  pub secret_sha256: Vec<u8>,
  pub created_at: OffsetDateTime,
  pub maybe_last_event_at: Option<OffsetDateTime>,
}

impl Database {
  /// The hook, if one is set.
  pub async fn provider_event_hook(&self) -> Result<Option<ProviderEventHook>, DbError> {
    let row = sqlx::query!(
      "SELECT secret_sha256, created_at, last_event_at FROM identity_provider_event_hook WHERE id"
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| ProviderEventHook {
      secret_sha256: row.secret_sha256,
      created_at: row.created_at,
      maybe_last_event_at: row.last_event_at,
    }))
  }

  /// Sets a new secret, replacing the old one at once.
  pub async fn put_provider_event_secret(&self, secret_sha256: &[u8], by: UserId) -> Result<(), DbError> {
    sqlx::query!(
      "INSERT INTO identity_provider_event_hook (id, secret_sha256, created_by) VALUES (true, $1, $2)
       ON CONFLICT (id) DO UPDATE
         SET secret_sha256 = EXCLUDED.secret_sha256, created_by = EXCLUDED.created_by,
             created_at = now(), last_event_at = NULL",
      secret_sha256,
      to_db(by),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Removes the hook; `false` if there was none.
  pub async fn delete_provider_event_hook(&self) -> Result<bool, DbError> {
    let result = sqlx::query!("DELETE FROM identity_provider_event_hook WHERE id")
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected() > 0)
  }

  /// An accepted event arrived now.
  pub async fn note_provider_event(&self) -> Result<(), DbError> {
    sqlx::query!("UPDATE identity_provider_event_hook SET last_event_at = now() WHERE id")
      .execute(self.pool())
      .await?;
    Ok(())
  }
}
