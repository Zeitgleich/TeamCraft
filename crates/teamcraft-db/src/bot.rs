//! Bot accounts: users that authenticate with a token instead of the identity provider.

use teamcraft_proto::UserId;
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// The identity provider name bot accounts are stored under.
pub const BOT_ISSUER: &str = "teamcraft-bot";

#[derive(Debug, Clone)]
pub struct StoredBot {
  pub user: UserId,
  pub name: String,
  pub maybe_description: Option<String>,
  pub created_at: OffsetDateTime,
  pub is_revoked: bool,
}

impl Database {
  /// Creates the bot's user and the bot record. `token_hash` is the SHA-256 of the token.
  pub async fn create_bot(
    &self,
    user: UserId,
    name: &str,
    maybe_description: Option<&str>,
    token_hash: &[u8],
    created_by: UserId,
  ) -> Result<StoredBot, DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      r#"INSERT INTO users (id, oidc_issuer, oidc_subject, display_name, kind, oidc_role)
         VALUES ($1, $2, $3, $4, 'bot', 'guest')"#,
      to_db(user),
      BOT_ISSUER,
      user.to_string(),
      name,
    )
    .execute(&mut *tx)
    .await?;
    let row = sqlx::query!(
      r#"INSERT INTO bots (user_id, name, description, token_hash, created_by)
         VALUES ($1, $2, $3, $4, $5) RETURNING created_at"#,
      to_db(user),
      name,
      maybe_description,
      token_hash,
      to_db(created_by),
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(StoredBot {
      user,
      name: name.to_owned(),
      maybe_description: maybe_description.map(str::to_owned),
      created_at: row.created_at,
      is_revoked: false,
    })
  }

  /// The active bot whose token has this hash.
  pub async fn find_bot_by_token_hash(&self, token_hash: &[u8]) -> Result<Option<StoredBot>, DbError> {
    let row = sqlx::query!(
      r#"SELECT user_id, name, description, created_at FROM bots
         WHERE token_hash = $1 AND revoked_at IS NULL"#,
      token_hash,
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(StoredBot {
          user: from_db(row.user_id)?,
          name: row.name,
          maybe_description: row.description,
          created_at: row.created_at,
          is_revoked: false,
        })
      })
      .transpose()
  }

  pub async fn get_bot(&self, user: UserId) -> Result<Option<StoredBot>, DbError> {
    let row = sqlx::query!(
      r#"SELECT user_id, name, description, created_at, revoked_at IS NOT NULL AS "is_revoked!"
         FROM bots WHERE user_id = $1"#,
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(StoredBot {
          user: from_db(row.user_id)?,
          name: row.name,
          maybe_description: row.description,
          created_at: row.created_at,
          is_revoked: row.is_revoked,
        })
      })
      .transpose()
  }

  pub async fn list_bots(&self) -> Result<Vec<StoredBot>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT user_id, name, description, created_at, revoked_at IS NOT NULL AS "is_revoked!"
         FROM bots ORDER BY created_at, user_id"#,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredBot {
          user: from_db(row.user_id)?,
          name: row.name,
          maybe_description: row.description,
          created_at: row.created_at,
          is_revoked: row.is_revoked,
        })
      })
      .collect()
  }

  /// Replaces the token of an active bot; the old token stops working. `false` if there is none.
  pub async fn replace_bot_token(&self, user: UserId, token_hash: &[u8]) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE bots SET token_hash = $2 WHERE user_id = $1 AND revoked_at IS NULL",
      to_db(user),
      token_hash,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// Marks the bot revoked; `false` if it was not active.
  pub async fn revoke_bot(&self, user: UserId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE bots SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }
}
