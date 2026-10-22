//! Local guest accounts and server-side sessions.

use teamcraft_proto::UserId;
use time::{Duration, OffsetDateTime};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// The identity provider name local accounts are stored under.
pub const LOCAL_ISSUER: &str = "teamcraft-local";

/// Sessions are only written to this often; the idle timeout is therefore accurate to this much.
const TOUCH_INTERVAL: Duration = Duration::seconds(30);

#[derive(Debug, Clone)]
pub struct LocalAccount {
  pub user: UserId,
  pub login: String,
  pub maybe_password_hash: Option<String>,
  pub maybe_totp_secret_sealed: Option<Vec<u8>>,
  pub is_totp_confirmed: bool,
  pub maybe_totp_last_step: Option<i64>,
  pub failed_attempts: i32,
  pub maybe_locked_until: Option<OffsetDateTime>,
}

/// Where a session stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
  Active,
  /// Past its length or idle limit.
  Expired,
  /// The server never saw it (or it was ended) and `create_if_missing` was off.
  Unknown,
}

/// Limits a session is checked against; `None` means no limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionLimits {
  pub maybe_max_age: Option<Duration>,
  pub maybe_idle_timeout: Option<Duration>,
}

impl Database {
  /// Creates the guest's user and an account that waits for activation. `false` if the login is
  /// taken.
  pub async fn create_local_account(
    &self,
    user: UserId,
    login: &str,
    display_name: &str,
    activation_digest: &[u8],
    activation_expires_at: OffsetDateTime,
  ) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      r#"INSERT INTO users (id, oidc_issuer, oidc_subject, email, display_name, kind, oidc_role)
         VALUES ($1, $2, $3, $4, $5, 'guest', 'guest')"#,
      to_db(user),
      LOCAL_ISSUER,
      user.to_string(),
      login,
      display_name,
    )
    .execute(&mut *tx)
    .await?;
    let inserted = sqlx::query!(
      r#"INSERT INTO local_accounts (user_id, login, activation_digest, activation_expires_at)
         VALUES ($1, $2, $3, $4) ON CONFLICT (login) DO NOTHING"#,
      to_db(user),
      login,
      activation_digest,
      activation_expires_at,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
      tx.rollback().await?;
      return Ok(false);
    }
    tx.commit().await?;
    Ok(true)
  }

  pub async fn find_local_account(&self, login: &str) -> Result<Option<LocalAccount>, DbError> {
    let row = sqlx::query!(
      r#"SELECT user_id, login, password_hash, totp_secret_sealed, is_totp_confirmed, totp_last_step,
                failed_attempts, locked_until
         FROM local_accounts WHERE login = $1"#,
      login,
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(LocalAccount {
          user: from_db(row.user_id)?,
          login: row.login,
          maybe_password_hash: row.password_hash,
          maybe_totp_secret_sealed: row.totp_secret_sealed,
          is_totp_confirmed: row.is_totp_confirmed,
          maybe_totp_last_step: row.totp_last_step,
          failed_attempts: row.failed_attempts,
          maybe_locked_until: row.locked_until,
        })
      })
      .transpose()
  }

  pub async fn get_local_account(&self, user: UserId) -> Result<Option<LocalAccount>, DbError> {
    let row = sqlx::query!("SELECT login FROM local_accounts WHERE user_id = $1", to_db(user))
      .fetch_optional(self.pool())
      .await?;
    match row {
      Some(row) => self.find_local_account(&row.login).await,
      None => Ok(None),
    }
  }

  /// The account whose activation token has this digest and has not expired.
  pub async fn find_local_account_by_activation(
    &self,
    activation_digest: &[u8],
    now: OffsetDateTime,
  ) -> Result<Option<LocalAccount>, DbError> {
    let row = sqlx::query!(
      "SELECT login FROM local_accounts WHERE activation_digest = $1 AND activation_expires_at > $2",
      activation_digest,
      now,
    )
    .fetch_optional(self.pool())
    .await?;
    match row {
      Some(row) => self.find_local_account(&row.login).await,
      None => Ok(None),
    }
  }

  /// Locks the account out until the guest follows a new activation link: password and TOTP are
  /// cleared. `false` if there is no such account.
  pub async fn reissue_local_activation(
    &self,
    user: UserId,
    activation_digest: &[u8],
    activation_expires_at: OffsetDateTime,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE local_accounts
         SET password_hash = NULL, totp_secret_sealed = NULL, is_totp_confirmed = false,
             totp_last_step = NULL, failed_attempts = 0, locked_until = NULL,
             activation_digest = $2, activation_expires_at = $3
         WHERE user_id = $1"#,
      to_db(user),
      activation_digest,
      activation_expires_at,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Spends the activation token: sets the password and the (unconfirmed) TOTP secret. The user
  /// and login, or `None` if the token is unknown, used or expired.
  pub async fn activate_local_account(
    &self,
    activation_digest: &[u8],
    now: OffsetDateTime,
    password_hash: &str,
    totp_secret_sealed: &[u8],
  ) -> Result<Option<(UserId, String)>, DbError> {
    let row = sqlx::query!(
      r#"UPDATE local_accounts
         SET password_hash = $3, totp_secret_sealed = $4, is_totp_confirmed = false,
             activation_digest = NULL, activation_expires_at = NULL
         WHERE activation_digest = $1 AND activation_expires_at > $2
         RETURNING user_id, login"#,
      activation_digest,
      now,
      password_hash,
      totp_secret_sealed,
    )
    .fetch_optional(self.pool())
    .await?;
    row.map(|row| Ok((from_db(row.user_id)?, row.login))).transpose()
  }

  /// Counts a failed sign-in and locks the account for `lock` after `max_attempts` in a row.
  pub async fn record_login_failure(
    &self,
    user: UserId,
    max_attempts: i32,
    lock_until: OffsetDateTime,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE local_accounts
         SET failed_attempts = CASE WHEN failed_attempts + 1 >= $2 THEN 0 ELSE failed_attempts + 1 END,
             locked_until = CASE WHEN failed_attempts + 1 >= $2 THEN $3 ELSE locked_until END
         WHERE user_id = $1"#,
      to_db(user),
      max_attempts,
      lock_until,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn clear_login_failures(&self, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE local_accounts SET failed_attempts = 0, locked_until = NULL WHERE user_id = $1",
      to_db(user)
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Accepts a TOTP time step once: `false` if this step or a later one was used already.
  pub async fn use_totp_step(&self, user: UserId, step: i64, is_confirming: bool) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE local_accounts
         SET totp_last_step = $2, is_totp_confirmed = is_totp_confirmed OR $3
         WHERE user_id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)"#,
      to_db(user),
      step,
      is_confirming,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  pub async fn set_local_password(&self, user: UserId, password_hash: &str) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE local_accounts SET password_hash = $2 WHERE user_id = $1",
      to_db(user),
      password_hash
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Replaces the TOTP secret; the new one must be confirmed with a code.
  pub async fn reset_local_totp(&self, user: UserId, totp_secret_sealed: &[u8]) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE local_accounts
         SET totp_secret_sealed = $2, is_totp_confirmed = false, totp_last_step = NULL
         WHERE user_id = $1"#,
      to_db(user),
      totp_secret_sealed,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Starts (or restarts) a session at `now`.
  pub async fn start_session(
    &self,
    issuer: &str,
    subject: &str,
    session_key: &str,
    now: OffsetDateTime,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO session_activity (issuer, subject, session_key, started_at, last_seen_at)
         VALUES ($1, $2, $3, $4, $4)
         ON CONFLICT (issuer, subject, session_key) DO UPDATE SET started_at = $4, last_seen_at = $4"#,
      issuer,
      subject,
      session_key,
      now,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Checks a session against `limits` and records the activity. With `create_if_missing` the first
  /// request of a session the server has not seen starts it (identity provider sessions).
  pub async fn touch_session(
    &self,
    issuer: &str,
    subject: &str,
    session_key: &str,
    now: OffsetDateTime,
    limits: SessionLimits,
    create_if_missing: bool,
  ) -> Result<SessionState, DbError> {
    let mut row = self.session_times(issuer, subject, session_key).await?;
    if row.is_none() {
      if !create_if_missing {
        return Ok(SessionState::Unknown);
      }
      self.start_session(issuer, subject, session_key, now).await?;
      row = Some((now, now));
    }
    let Some((started_at, last_seen_at)) = row else {
      return Ok(SessionState::Unknown);
    };
    let too_old = limits.maybe_max_age.is_some_and(|max| now - started_at > max);
    let too_idle = limits
      .maybe_idle_timeout
      .is_some_and(|idle| now - last_seen_at > idle);
    if too_old || too_idle {
      return Ok(SessionState::Expired);
    }
    if now - last_seen_at > TOUCH_INTERVAL {
      sqlx::query!(
        r#"UPDATE session_activity SET last_seen_at = $4
           WHERE issuer = $1 AND subject = $2 AND session_key = $3"#,
        issuer,
        subject,
        session_key,
        now,
      )
      .execute(self.pool())
      .await?;
    }
    Ok(SessionState::Active)
  }

  pub async fn end_session(&self, issuer: &str, subject: &str, session_key: &str) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM session_activity WHERE issuer = $1 AND subject = $2 AND session_key = $3",
      issuer,
      subject,
      session_key
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Ends all sessions of a user (password change, deactivation). Returns how many.
  pub async fn end_sessions_of(&self, issuer: &str, subject: &str) -> Result<u64, DbError> {
    let result = sqlx::query!(
      "DELETE FROM session_activity WHERE issuer = $1 AND subject = $2",
      issuer,
      subject
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected())
  }

  /// Forgets sessions that have not been seen since `before`. Returns how many.
  pub async fn delete_stale_sessions(&self, before: OffsetDateTime) -> Result<u64, DbError> {
    let result = sqlx::query!("DELETE FROM session_activity WHERE last_seen_at < $1", before)
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected())
  }

  async fn session_times(
    &self,
    issuer: &str,
    subject: &str,
    session_key: &str,
  ) -> Result<Option<(OffsetDateTime, OffsetDateTime)>, DbError> {
    let row = sqlx::query!(
      r#"SELECT started_at, last_seen_at FROM session_activity
         WHERE issuer = $1 AND subject = $2 AND session_key = $3"#,
      issuer,
      subject,
      session_key,
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| (row.started_at, row.last_seen_at)))
  }
}
