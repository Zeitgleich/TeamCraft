use teamcraft_proto::{Id, Snowflake, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;

/// Whether a user is an employee or an external guest (ARCHITECTURE.md §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserKind {
  Member,
  Guest,
}

/// Identity attested by the OIDC provider.
#[derive(Debug, Clone)]
pub struct OidcIdentity<'a> {
  pub issuer: &'a str,
  pub subject: &'a str,
  pub maybe_username: Option<&'a str>,
  pub display_name: &'a str,
  pub kind: UserKind,
}

#[derive(Debug, Clone)]
pub struct User {
  pub id: UserId,
  pub maybe_username: Option<String>,
  pub display_name: String,
  pub kind: UserKind,
  pub created_at: OffsetDateTime,
  pub is_deactivated: bool,
}

impl User {
  fn from_row(
    id: i64,
    maybe_username: Option<String>,
    display_name: String,
    kind: &str,
    created_at: OffsetDateTime,
    is_deactivated: bool,
  ) -> Result<Self, DbError> {
    let snowflake = Snowflake::new(id).map_err(|e| DbError::InvalidStoredValue(e.to_string()))?;
    Ok(Self {
      id: UserId::from_snowflake(snowflake),
      maybe_username,
      display_name,
      kind: UserKind::from_db(kind)?,
      created_at,
      is_deactivated,
    })
  }
}

impl UserKind {
  fn as_db(self) -> &'static str {
    match self {
      Self::Member => "member",
      Self::Guest => "guest",
    }
  }

  fn from_db(value: &str) -> Result<Self, DbError> {
    match value {
      "member" => Ok(Self::Member),
      "guest" => Ok(Self::Guest),
      other => Err(DbError::InvalidStoredValue(format!("user kind {other:?}"))),
    }
  }
}

impl Database {
  /// Like [`Self::upsert_oidc_user`], but only writes when the user is new or the attested data
  /// changed. Meant for the per-request authentication path, which must not write every time.
  pub async fn sync_oidc_user(&self, identity: &OidcIdentity<'_>, new_id: UserId) -> Result<User, DbError> {
    let existing = sqlx::query!(
      r#"SELECT id, username, display_name, kind, created_at, deactivated_at IS NOT NULL AS "is_deactivated!"
         FROM users WHERE oidc_issuer = $1 AND oidc_subject = $2"#,
      identity.issuer,
      identity.subject,
    )
    .fetch_optional(self.pool())
    .await?;

    if let Some(row) = existing {
      let user = User::from_row(
        row.id,
        row.username,
        row.display_name,
        &row.kind,
        row.created_at,
        row.is_deactivated,
      )?;
      let is_unchanged = user.maybe_username.as_deref() == identity.maybe_username
        && user.display_name == identity.display_name
        && user.kind == identity.kind;
      if is_unchanged {
        return Ok(user);
      }
    }
    self.upsert_oidc_user(identity, new_id).await
  }

  /// Creates the user on first login and refreshes username, display name and kind afterwards.
  /// `new_id` is only used when the user does not exist yet.
  pub async fn upsert_oidc_user(&self, identity: &OidcIdentity<'_>, new_id: UserId) -> Result<User, DbError> {
    let row = sqlx::query!(
      r#"INSERT INTO users (id, oidc_issuer, oidc_subject, username, display_name, kind)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (oidc_issuer, oidc_subject) DO UPDATE
           SET username = EXCLUDED.username,
               display_name = EXCLUDED.display_name,
               kind = EXCLUDED.kind
         RETURNING id, username, display_name, kind, created_at, deactivated_at IS NOT NULL AS "is_deactivated!""#,
      new_id.snowflake().get(),
      identity.issuer,
      identity.subject,
      identity.maybe_username,
      identity.display_name,
      identity.kind.as_db(),
    )
    .fetch_one(self.pool())
    .await?;

    User::from_row(
      row.id,
      row.username,
      row.display_name,
      &row.kind,
      row.created_at,
      row.is_deactivated,
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn user_kind_maps_to_check_constraint_values() {
    assert_eq!(
      UserKind::from_db(UserKind::Member.as_db()).unwrap(),
      UserKind::Member
    );
    assert_eq!(
      UserKind::from_db(UserKind::Guest.as_db()).unwrap(),
      UserKind::Guest
    );
    assert!(UserKind::from_db("robot").is_err());
  }
}
