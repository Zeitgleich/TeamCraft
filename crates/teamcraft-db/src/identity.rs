use teamcraft_proto::{Snowflake, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// A user's public identity key as uploaded. The server stores it without judging it.
#[derive(Debug, Clone)]
pub struct IdentityKey {
  pub user_id: UserId,
  pub public_key: Vec<u8>,
  pub signature: Vec<u8>,
  pub maybe_attestation: Option<String>,
  pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct NewIdentityKey<'a> {
  pub id: Snowflake,
  pub user_id: UserId,
  pub public_key: &'a [u8],
  pub signature: &'a [u8],
  pub maybe_attestation: Option<&'a str>,
  /// Revoke the current key and store this one instead.
  pub replace: bool,
  /// The identity's first key bag, stored in the same transaction (e2ee.md §3.2).
  pub key_bag: InitialKeyBag<'a>,
}

#[derive(Debug, Clone)]
pub struct InitialKeyBag<'a> {
  pub encrypted_key_bag: &'a [u8],
  pub wrapped_backup_keys: &'a [u8],
}

/// What `put_identity_key` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityKeyOutcome {
  /// The key and its key bag are stored; the bag has this version.
  Stored { key_bag_version: u64 },
  /// A key exists and `replace` was not set.
  AlreadyExists,
}

impl Database {
  pub async fn put_identity_key(&self, new: &NewIdentityKey<'_>) -> Result<IdentityKeyOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    if new.replace {
      sqlx::query!(
        "UPDATE identity_keys SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        to_db(new.user_id),
      )
      .execute(&mut *tx)
      .await?;
    }
    // The partial unique index allows one active key per user; losing a race reports AlreadyExists.
    let inserted = sqlx::query!(
      r#"INSERT INTO identity_keys (id, user_id, public_key, signature, attestation)
         VALUES ($1, $2, $3, $4, $5) ON CONFLICT DO NOTHING"#,
      new.id.get(),
      to_db(new.user_id),
      new.public_key,
      new.signature,
      new.maybe_attestation,
    )
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
      return Ok(IdentityKeyOutcome::AlreadyExists);
    }
    // A bag left from an earlier identity can't be opened with the new one: the new bag replaces
    // it, as a newer version so devices holding the old one notice.
    let key_bag_version = sqlx::query_scalar!(
      r#"INSERT INTO key_bags (user_id, version, encrypted_key_bag, wrapped_backup_keys)
         VALUES ($1, 1, $2, $3)
         ON CONFLICT (user_id) DO UPDATE SET version = key_bags.version + 1,
           encrypted_key_bag = EXCLUDED.encrypted_key_bag,
           wrapped_backup_keys = EXCLUDED.wrapped_backup_keys, updated_at = now()
         RETURNING version"#,
      to_db(new.user_id),
      new.key_bag.encrypted_key_bag,
      new.key_bag.wrapped_backup_keys,
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(IdentityKeyOutcome::Stored {
      key_bag_version: u64::try_from(key_bag_version)
        .map_err(|_| DbError::InvalidStoredValue("key bag version".into()))?,
    })
  }

  pub async fn get_identity_key(&self, user: UserId) -> Result<Option<IdentityKey>, DbError> {
    let row = sqlx::query!(
      r#"SELECT user_id, public_key, signature, attestation, created_at FROM identity_keys
         WHERE user_id = $1 AND revoked_at IS NULL"#,
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(IdentityKey {
          user_id: from_db(row.user_id)?,
          public_key: row.public_key,
          signature: row.signature,
          maybe_attestation: row.attestation,
          created_at: row.created_at,
        })
      })
      .transpose()
  }
}
