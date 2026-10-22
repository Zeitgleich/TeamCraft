use teamcraft_proto::UserId;
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::to_db;

#[derive(Debug, Clone)]
pub struct KeyBag {
  pub version: u64,
  pub encrypted_key_bag: Vec<u8>,
  pub wrapped_backup_keys: Vec<u8>,
  pub updated_at: OffsetDateTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyBagOutcome {
  Stored {
    version: u64,
  },
  /// The stored version differs from the expected one (0 if there is none).
  VersionConflict {
    current_version: u64,
  },
}

impl Database {
  pub async fn get_key_bag(&self, user: UserId) -> Result<Option<KeyBag>, DbError> {
    let row = sqlx::query!(
      "SELECT version, encrypted_key_bag, wrapped_backup_keys, updated_at FROM key_bags WHERE user_id = $1",
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| KeyBag {
      version: u64::try_from(row.version).unwrap_or(0),
      encrypted_key_bag: row.encrypted_key_bag,
      wrapped_backup_keys: row.wrapped_backup_keys,
      updated_at: row.updated_at,
    }))
  }

  /// Compare-and-swap: creates the bag (`expected_version` 0) or replaces it if its version is still
  /// `expected_version`, in a single statement so concurrent writers can't both win.
  pub async fn put_key_bag(
    &self,
    user: UserId,
    expected_version: u64,
    encrypted_key_bag: &[u8],
    wrapped_backup_keys: &[u8],
  ) -> Result<KeyBagOutcome, DbError> {
    let expected =
      i64::try_from(expected_version).map_err(|_| DbError::InvalidStoredValue("version".into()))?;
    let stored = if expected == 0 {
      sqlx::query_scalar!(
        r#"INSERT INTO key_bags (user_id, version, encrypted_key_bag, wrapped_backup_keys)
           VALUES ($1, 1, $2, $3) ON CONFLICT DO NOTHING RETURNING version"#,
        to_db(user),
        encrypted_key_bag,
        wrapped_backup_keys,
      )
      .fetch_optional(self.pool())
      .await?
    } else {
      sqlx::query_scalar!(
        r#"UPDATE key_bags SET version = version + 1, encrypted_key_bag = $3, wrapped_backup_keys = $4,
                               updated_at = now()
           WHERE user_id = $1 AND version = $2 RETURNING version"#,
        to_db(user),
        expected,
        encrypted_key_bag,
        wrapped_backup_keys,
      )
      .fetch_optional(self.pool())
      .await?
    };
    if let Some(version) = stored {
      return Ok(KeyBagOutcome::Stored {
        version: u64::try_from(version).unwrap_or(0),
      });
    }
    let current = sqlx::query_scalar!("SELECT version FROM key_bags WHERE user_id = $1", to_db(user))
      .fetch_optional(self.pool())
      .await?;
    Ok(KeyBagOutcome::VersionConflict {
      current_version: current.map_or(0, |v| u64::try_from(v).unwrap_or(0)),
    })
  }
}
