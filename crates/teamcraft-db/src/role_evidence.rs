//! Identity provider tokens kept for the owners' apps (e2ee.md §3.1.3). Stored as they came: only
//! the apps verify them.

use teamcraft_proto::UserId;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// Tokens read at once; far beyond the owners and admins an instance has. Who is on the roster or has
/// a provider role is what is stored in the first place (`role_evidence::put`).
const MAX_EVIDENCE: i64 = 2000;

/// The newest token of a person with the identity key the server has for them.
#[derive(Debug, Clone)]
pub struct StoredRoleEvidence {
  pub user_id: UserId,
  pub token: String,
  pub public_key: Vec<u8>,
}

/// What the server's records say about somebody on the roster.
#[derive(Debug, Clone)]
pub struct RoleStanding {
  pub user_id: UserId,
  pub is_gone: bool,
  /// `None` for an account not from the identity provider.
  pub maybe_oidc_role: Option<String>,
}

impl Database {
  /// Keeps `token` if it is newer than the one stored. Returns whether it was.
  pub async fn put_role_evidence(&self, user: UserId, token: &str, issued_at: i64) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"INSERT INTO role_evidence (user_id, token, issued_at) VALUES ($1, $2, $3)
         ON CONFLICT (user_id) DO UPDATE SET token = EXCLUDED.token, issued_at = EXCLUDED.issued_at,
           updated_at = now()
         WHERE role_evidence.issued_at < EXCLUDED.issued_at"#,
      to_db(user),
      token,
      issued_at,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Every stored token of somebody with an active identity key.
  pub async fn list_role_evidence(&self) -> Result<Vec<StoredRoleEvidence>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT evidence.user_id, evidence.token, keys.public_key
         FROM role_evidence evidence
         JOIN identity_keys keys ON keys.user_id = evidence.user_id AND keys.revoked_at IS NULL
         ORDER BY evidence.updated_at DESC LIMIT $1"#,
      MAX_EVIDENCE,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredRoleEvidence {
          user_id: from_db(row.user_id)?,
          token: row.token,
          public_key: row.public_key,
        })
      })
      .collect()
  }

  /// The active accounts the provider makes owners, whose apps sign the roster.
  pub async fn list_provider_owners(&self) -> Result<Vec<UserId>, DbError> {
    let rows = sqlx::query_scalar!(
      "SELECT id FROM users WHERE oidc_role = 'owner' AND deactivated_at IS NULL ORDER BY id"
    )
    .fetch_all(self.pool())
    .await?;
    rows.into_iter().map(from_db).collect()
  }

  /// The server's records of `users`.
  pub async fn role_standings(&self, users: &[UserId]) -> Result<Vec<RoleStanding>, DbError> {
    let ids: Vec<i64> = users.iter().map(|user| to_db(*user)).collect();
    let rows = sqlx::query!(
      r#"SELECT id, deactivated_at IS NOT NULL AS "is_gone!", oidc_role
         FROM users WHERE id = ANY($1) ORDER BY id"#,
      &ids,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(RoleStanding {
          user_id: from_db(row.id)?,
          is_gone: row.is_gone,
          maybe_oidc_role: row.oidc_role,
        })
      })
      .collect()
  }
}
