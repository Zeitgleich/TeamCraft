//! Identity provider tokens kept for the owners' apps (e2ee.md §3.1.3). Stored as they came: only
//! the apps verify them.

use teamcraft_proto::UserId;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// Tokens read at once; far beyond the owners and admins an instance has. Who is on the roster or has
/// a provider role is what is stored in the first place (`role_evidence::put`).
const MAX_EVIDENCE: i64 = 2000;

/// Tokens of members with no role read at once, the longest waiting first: a page for one roster
/// (issue #1378). The next page follows once the roster that took these in settles them.
const MAX_ACCOUNT_EVIDENCE: i64 = 200;

/// The newest token of a person with the identity key the server has for them.
#[derive(Debug, Clone)]
pub struct StoredRoleEvidence {
  pub user_id: UserId,
  pub token: String,
  pub public_key: Vec<u8>,
}

/// A token showing that somebody lost a role, expired by now.
#[derive(Debug, Clone)]
pub struct StoredRoleLoss {
  pub user_id: UserId,
  pub token: String,
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
  /// Keeps `token` if it is newer than the one stored. `for_account`: the token of a member with no
  /// role, only for noting their account (issue #1378). Returns whether it was kept.
  pub async fn put_role_evidence(
    &self,
    user: UserId,
    token: &str,
    issued_at: i64,
    for_account: bool,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"INSERT INTO role_evidence (user_id, token, issued_at, for_account) VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id) DO UPDATE SET token = EXCLUDED.token, issued_at = EXCLUDED.issued_at,
           for_account = EXCLUDED.for_account, updated_at = now()
         WHERE role_evidence.issued_at < EXCLUDED.issued_at"#,
      to_db(user),
      token,
      issued_at,
      for_account,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// How many members' tokens wait for their account to be noted.
  pub async fn count_account_evidence(&self) -> Result<i64, DbError> {
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM role_evidence WHERE for_account"#)
      .fetch_one(self.pool())
      .await?;
    Ok(count)
  }

  /// The stored tokens of people with an active identity key: every role holder's, newest first, and a
  /// page of members' ([`MAX_ACCOUNT_EVIDENCE`]), the longest waiting first, so that a large workspace
  /// is taken in a page per roster and never crowds out a role holder.
  pub async fn list_role_evidence(&self) -> Result<Vec<StoredRoleEvidence>, DbError> {
    let rows = sqlx::query!(
      r#"(SELECT evidence.user_id AS "user_id!", evidence.token AS "token!", keys.public_key AS "public_key!"
          FROM role_evidence evidence
          JOIN identity_keys keys ON keys.user_id = evidence.user_id AND keys.revoked_at IS NULL
          WHERE NOT evidence.for_account
          ORDER BY evidence.updated_at DESC LIMIT $1)
         UNION ALL
         (SELECT evidence.user_id, evidence.token, keys.public_key
          FROM role_evidence evidence
          JOIN identity_keys keys ON keys.user_id = evidence.user_id AND keys.revoked_at IS NULL
          WHERE evidence.for_account
          ORDER BY evidence.updated_at ASC LIMIT $2)"#,
      MAX_EVIDENCE,
      MAX_ACCOUNT_EVIDENCE,
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

  /// Keeps the token showing that `user` lost a role, unless one is stored already that isn't a day
  /// older. A token that always made way for the next would never expire, and only an expired one is
  /// handed out, while the person's app refreshes every few minutes. Returns whether it was kept.
  pub async fn put_role_loss(
    &self,
    user: UserId,
    token: &str,
    issued_at: i64,
    expires_at: i64,
    lost_to: &str,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"INSERT INTO role_losses (user_id, token, issued_at, expires_at, lost_to) VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (user_id) DO UPDATE SET token = EXCLUDED.token, issued_at = EXCLUDED.issued_at,
           expires_at = EXCLUDED.expires_at, lost_to = EXCLUDED.lost_to, updated_at = now()
         WHERE role_losses.issued_at < EXCLUDED.issued_at - 86400"#,
      to_db(user),
      token,
      issued_at,
      expires_at,
      lost_to,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// The tokens that have expired at `now` (seconds).
  pub async fn list_role_losses(&self, now: i64) -> Result<Vec<StoredRoleLoss>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT user_id, token FROM role_losses WHERE expires_at <= $1
         ORDER BY updated_at DESC LIMIT $2"#,
      now,
      MAX_EVIDENCE,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredRoleLoss {
          user_id: from_db(row.user_id)?,
          token: row.token,
        })
      })
      .collect()
  }

  /// A token of `user` from `issued_at` was accepted and holds a higher role than any of `lost_to`: a
  /// loss from before it is over.
  pub async fn delete_regained_role_loss(
    &self,
    user: UserId,
    issued_at: i64,
    lost_to: &[&str],
  ) -> Result<(), DbError> {
    let lost_to: Vec<String> = lost_to.iter().map(|role| (*role).to_owned()).collect();
    sqlx::query!(
      "DELETE FROM role_losses WHERE user_id = $1 AND issued_at <= $2 AND lost_to = ANY($3)",
      to_db(user),
      issued_at,
      &lost_to,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// A token of `user` from `issued_at` is stored: a loss from before it is over.
  pub async fn delete_older_role_loss(&self, user: UserId, issued_at: i64) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM role_losses WHERE user_id = $1 AND issued_at < $2",
      to_db(user),
      issued_at,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// The signed roster took tokens of these people up to each one's time into account: they are of no
  /// more use, and they name the person. One statement each, however many marks a roster carries.
  pub async fn settle_role_evidence(&self, marks: &[(UserId, i64)]) -> Result<(), DbError> {
    let users: Vec<i64> = marks.iter().map(|(user, _)| to_db(*user)).collect();
    let times: Vec<i64> = marks.iter().map(|(_, issued_at)| *issued_at).collect();
    sqlx::query!(
      r#"DELETE FROM role_evidence evidence
         USING UNNEST($1::bigint[], $2::bigint[]) AS mark(user_id, issued_at)
         WHERE evidence.user_id = mark.user_id AND evidence.issued_at <= mark.issued_at"#,
      &users,
      &times,
    )
    .execute(self.pool())
    .await?;
    sqlx::query!(
      r#"DELETE FROM role_losses loss
         USING UNNEST($1::bigint[], $2::bigint[]) AS mark(user_id, issued_at)
         WHERE loss.user_id = mark.user_id AND loss.issued_at <= mark.issued_at"#,
      &users,
      &times,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Drops what nobody took up in `days`: no owner's app acts on tokens that old anyway.
  pub async fn prune_role_evidence(&self, days: i32) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM role_evidence WHERE updated_at < now() - make_interval(days => $1)",
      days,
    )
    .execute(self.pool())
    .await?;
    sqlx::query!(
      "DELETE FROM role_losses WHERE updated_at < now() - make_interval(days => $1)",
      days,
    )
    .execute(self.pool())
    .await?;
    Ok(())
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
