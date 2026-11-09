use std::collections::{HashMap, HashSet};

use teamcraft_proto::{ManualPresence, UserId, UserKind};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};

/// Identity attested by the OIDC provider.
#[derive(Debug, Clone)]
pub struct OidcIdentity<'a> {
  pub issuer: &'a str,
  pub subject: &'a str,
  pub maybe_username: Option<&'a str>,
  pub maybe_email: Option<&'a str>,
  pub display_name: &'a str,
  pub kind: UserKind,
  /// The TeamCraft role the token granted (stored for the OIDC sync to compare against).
  pub maybe_oidc_role: Option<&'a str>,
  /// The organization claim of the token, if the server reads one.
  pub maybe_organization: Option<&'a str>,
  /// Whether `maybe_organization` is authoritative: a token without the claim then clears the
  /// stored organization. `false` leaves the stored value alone (the server reads no claim).
  pub should_sync_organization: bool,
}

#[derive(Debug, Clone)]
pub struct User {
  pub id: UserId,
  pub maybe_username: Option<String>,
  pub display_name: String,
  pub kind: UserKind,
  pub created_at: OffsetDateTime,
  pub is_deactivated: bool,
  /// The account was deleted (issue #630): a tombstone that only names old messages. Always
  /// deactivated too.
  pub is_deleted: bool,
  /// Display label from the identity provider; no permission depends on it.
  pub maybe_organization: Option<String>,
}

/// How a user chose to appear (ARCHITECTURE.md §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresencePreference {
  pub manual: ManualPresence,
  /// When the manual choice ends and presence is automatic again (column `dnd_until`, named when
  /// only do-not-disturb could end).
  pub maybe_until: Option<OffsetDateTime>,
}

/// A user after sign-in, with whether the default memberships still have to be applied.
#[derive(Debug, Clone)]
pub struct SyncedUser {
  pub user: User,
  pub is_awaiting_default_memberships: bool,
  /// The role the provider's last token showed, when this one shows another.
  pub maybe_previous_role: Option<String>,
  /// A token showing that this account lost a role is kept (role evidence).
  pub has_role_loss: bool,
}

/// Filter of a directory search.
#[derive(Debug, Clone)]
pub struct UserSearch<'a> {
  pub viewer: UserId,
  pub query: &'a str,
  /// Guests only find people who share a conversation with them.
  pub is_restricted_to_shared: bool,
  /// Only people of this organization (exact match).
  pub maybe_organization: Option<&'a str>,
  pub limit: i64,
}

impl Database {
  /// Creates the user on first login and refreshes username, email, display name and kind afterwards.
  /// `new_id` is only used when the user does not exist yet.
  pub async fn upsert_oidc_user(&self, identity: &OidcIdentity<'_>, new_id: UserId) -> Result<User, DbError> {
    Ok(self.upsert_oidc_user_flagged(identity, new_id).await?.user)
  }

  async fn upsert_oidc_user_flagged(
    &self,
    identity: &OidcIdentity<'_>,
    new_id: UserId,
  ) -> Result<SyncedUser, DbError> {
    let row = sqlx::query!(
      r#"INSERT INTO users (id, oidc_issuer, oidc_subject, username, email, display_name, idp_display_name, kind, oidc_role, oidc_role_seen, organization)
         VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $8, $10)
         ON CONFLICT (oidc_issuer, oidc_subject) DO UPDATE
           SET username = EXCLUDED.username,
               email = EXCLUDED.email,
               display_name = COALESCE(users.display_name_override, EXCLUDED.display_name),
               idp_display_name = EXCLUDED.idp_display_name,
               kind = EXCLUDED.kind,
               oidc_role = EXCLUDED.oidc_role,
               oidc_role_seen = EXCLUDED.oidc_role,
               organization = CASE WHEN $9 THEN EXCLUDED.organization ELSE users.organization END
         RETURNING id, username, display_name, kind, created_at, organization, deactivated_at IS NOT NULL AS "is_deactivated!", deleted_at IS NOT NULL AS "is_deleted!",
                   default_memberships_at IS NULL AS "is_awaiting_defaults!""#,
      to_db(new_id),
      identity.issuer,
      identity.subject,
      identity.maybe_username,
      identity.maybe_email,
      identity.display_name,
      identity.kind.as_str(),
      identity.maybe_oidc_role,
      identity.should_sync_organization,
      identity.maybe_organization,
    )
    .fetch_one(self.pool())
    .await?;
    Ok(SyncedUser {
      user: user_from_parts(
        row.id,
        row.username,
        row.display_name,
        row.kind,
        row.created_at,
        row.organization,
        AccountFlags {
          is_deactivated: row.is_deactivated,
          is_deleted: row.is_deleted,
        },
      )?,
      is_awaiting_default_memberships: row.is_awaiting_defaults,
      maybe_previous_role: None,
      has_role_loss: false,
    })
  }

  /// Like [`Self::upsert_oidc_user`], but only writes when the user is new or the attested data
  /// changed. Meant for the per-request authentication path, which must not write every time.
  pub async fn sync_oidc_user(&self, identity: &OidcIdentity<'_>, new_id: UserId) -> Result<User, DbError> {
    Ok(self.sync_oidc_user_flagged(identity, new_id).await?.user)
  }

  /// [`Self::sync_oidc_user`] that also says whether the account still waits for its default
  /// memberships, from the same read, so the per-request path needs no extra query.
  pub async fn sync_oidc_user_flagged(
    &self,
    identity: &OidcIdentity<'_>,
    new_id: UserId,
  ) -> Result<SyncedUser, DbError> {
    let existing = sqlx::query!(
      r#"SELECT id, username, email, display_name, COALESCE(idp_display_name, display_name) AS "idp_display_name!", kind, oidc_role, COALESCE(oidc_role_seen, oidc_role) AS seen_role, created_at, organization, deactivated_at IS NOT NULL AS "is_deactivated!", deleted_at IS NOT NULL AS "is_deleted!",
                default_memberships_at IS NULL AS "is_awaiting_defaults!",
                EXISTS (SELECT 1 FROM role_losses WHERE role_losses.user_id = users.id) AS "has_role_loss!"
         FROM users WHERE oidc_issuer = $1 AND oidc_subject = $2"#,
      identity.issuer,
      identity.subject,
    )
    .fetch_optional(self.pool())
    .await?;

    if let Some(row) = existing {
      let is_unchanged = row.username.as_deref() == identity.maybe_username
        && row.email.as_deref() == identity.maybe_email
        // The provider's name, not the shown one: an override is not a change to sync.
        && row.idp_display_name == identity.display_name
        && row.kind == identity.kind.as_str()
        && row.oidc_role.as_deref() == identity.maybe_oidc_role
        && row.seen_role.as_deref() == identity.maybe_oidc_role
        && (!identity.should_sync_organization || row.organization.as_deref() == identity.maybe_organization);
      if is_unchanged {
        return Ok(SyncedUser {
          user: user_from_parts(
            row.id,
            row.username,
            row.display_name,
            row.kind,
            row.created_at,
            row.organization,
            AccountFlags {
              is_deactivated: row.is_deactivated,
              is_deleted: row.is_deleted,
            },
          )?,
          is_awaiting_default_memberships: row.is_awaiting_defaults,
          maybe_previous_role: None,
          has_role_loss: row.has_role_loss,
        });
      }
      let maybe_previous_role = row
        .seen_role
        .filter(|role| Some(role.as_str()) != identity.maybe_oidc_role);
      let mut synced = self.upsert_oidc_user_flagged(identity, new_id).await?;
      synced.maybe_previous_role = maybe_previous_role;
      synced.has_role_loss = row.has_role_loss;
      return Ok(synced);
    }
    self.upsert_oidc_user_flagged(identity, new_id).await
  }

  /// The role the identity provider gave the user at their last sign-in (`None`: no such user;
  /// `Some(None)`: a user without a role, e.g. a local guest).
  pub async fn get_user_oidc_role(&self, id: UserId) -> Result<Option<Option<String>>, DbError> {
    let row = sqlx::query!("SELECT oidc_role FROM users WHERE id = $1", to_db(id))
      .fetch_optional(self.pool())
      .await?;
    Ok(row.map(|row| row.oidc_role))
  }

  /// The name the member chose instead of the provider's (`None`: the provider's name is shown).
  pub async fn get_display_name_override(&self, id: UserId) -> Result<Option<String>, DbError> {
    let row = sqlx::query!("SELECT display_name_override FROM users WHERE id = $1", to_db(id))
      .fetch_optional(self.pool())
      .await?;
    Ok(row.and_then(|row| row.display_name_override))
  }

  /// Sets the member's own name, or with `None` clears it so the provider's name shows again.
  pub async fn set_display_name_override(&self, id: UserId, maybe_name: Option<&str>) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE users
         SET idp_display_name = COALESCE(idp_display_name, display_name),
             display_name_override = $2,
             display_name = COALESCE($2, idp_display_name, display_name)
         WHERE id = $1"#,
      to_db(id),
      maybe_name,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// The role the user's last sign-in brought, for a decision made with nobody signed in.
  pub async fn last_oidc_role(&self, id: UserId) -> Result<Option<String>, DbError> {
    let maybe_role = sqlx::query_scalar!("SELECT oidc_role FROM users WHERE id = $1", to_db(id))
      .fetch_optional(self.pool())
      .await?;
    Ok(maybe_role.flatten())
  }

  pub async fn get_user(&self, id: UserId) -> Result<Option<User>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, username, display_name, kind, created_at, organization,
                deactivated_at IS NOT NULL AS "is_deactivated!", deleted_at IS NOT NULL AS "is_deleted!"
         FROM users WHERE id = $1"#,
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|r| {
        user_from_parts(
          r.id,
          r.username,
          r.display_name,
          r.kind,
          r.created_at,
          r.organization,
          AccountFlags {
            is_deactivated: r.is_deactivated,
            is_deleted: r.is_deleted,
          },
        )
      })
      .transpose()
  }

  pub async fn get_users(&self, ids: &[UserId]) -> Result<Vec<User>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, username, display_name, kind, created_at, organization,
                deactivated_at IS NOT NULL AS "is_deactivated!", deleted_at IS NOT NULL AS "is_deleted!"
         FROM users WHERE id = ANY($1) ORDER BY id"#,
      &to_db_all(ids),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|r| {
        user_from_parts(
          r.id,
          r.username,
          r.display_name,
          r.kind,
          r.created_at,
          r.organization,
          AccountFlags {
            is_deactivated: r.is_deactivated,
            is_deleted: r.is_deleted,
          },
        )
      })
      .collect()
  }

  /// Directory search by name or username (case-insensitive substring), active users only.
  pub async fn search_users(&self, search: &UserSearch<'_>) -> Result<Vec<User>, DbError> {
    let pattern = format!("%{}%", escape_like(search.query));
    let rows = sqlx::query!(
      r#"SELECT u.id, u.username, u.display_name, u.kind, u.created_at, u.organization,
                u.deactivated_at IS NOT NULL AS "is_deactivated!", u.deleted_at IS NOT NULL AS "is_deleted!"
         FROM users u
         WHERE u.deactivated_at IS NULL
           AND (u.display_name ILIKE $2 OR u.username ILIKE $2)
           AND ($5::text IS NULL OR u.organization = $5)
           AND (NOT $3 OR EXISTS (
                 SELECT 1 FROM memberships a JOIN memberships b ON a.conversation_id = b.conversation_id
                 WHERE a.user_id = $1 AND b.user_id = u.id))
         ORDER BY u.display_name, u.id
         LIMIT $4"#,
      to_db(search.viewer),
      pattern,
      search.is_restricted_to_shared,
      search.limit,
      search.maybe_organization,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|r| {
        user_from_parts(
          r.id,
          r.username,
          r.display_name,
          r.kind,
          r.created_at,
          r.organization,
          AccountFlags {
            is_deactivated: r.is_deactivated,
            is_deleted: r.is_deleted,
          },
        )
      })
      .collect()
  }

  /// The organizations of active users, sorted, for the directory filter.
  pub async fn list_organizations(&self) -> Result<Vec<String>, DbError> {
    let rows = sqlx::query_scalar!(
      r#"SELECT DISTINCT organization AS "organization!" FROM users
         WHERE organization IS NOT NULL AND deactivated_at IS NULL ORDER BY 1"#
    )
    .fetch_all(self.pool())
    .await?;
    Ok(rows)
  }

  /// `true` if the two users share at least one conversation.
  pub async fn users_share_conversation(&self, a: UserId, b: UserId) -> Result<bool, DbError> {
    let found = sqlx::query_scalar!(
      r#"SELECT EXISTS (SELECT 1 FROM memberships x JOIN memberships y ON x.conversation_id = y.conversation_id
                       WHERE x.user_id = $1 AND y.user_id = $2) AS "found!""#,
      to_db(a),
      to_db(b),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(found)
  }

  /// Users that share at least one conversation with `user` (audience of presence updates).
  pub async fn users_sharing_conversation(&self, user: UserId) -> Result<HashSet<UserId>, DbError> {
    let rows = sqlx::query_scalar!(
      r#"SELECT DISTINCT b.user_id AS "id!"
         FROM memberships a JOIN memberships b ON a.conversation_id = b.conversation_id
         WHERE a.user_id = $1 AND b.user_id <> $1"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    rows.into_iter().map(from_db).collect()
  }

  pub async fn get_presence_preference(&self, user: UserId) -> Result<PresencePreference, DbError> {
    let row = sqlx::query!(
      "SELECT presence_pref, dnd_until FROM users WHERE id = $1",
      to_db(user)
    )
    .fetch_one(self.pool())
    .await?;
    Ok(PresencePreference {
      manual: ManualPresence::from(row.presence_pref),
      maybe_until: row.dnd_until,
    })
  }

  pub async fn set_presence_preference(
    &self,
    user: UserId,
    preference: &PresencePreference,
  ) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE users SET presence_pref = $2, dnd_until = $3 WHERE id = $1",
      to_db(user),
      preference.manual.as_str(),
      preference.maybe_until,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// The holiday region the user picked for their own series (#1441); `None`: the instance's.
  pub async fn holiday_region(&self, user: UserId) -> Result<Option<String>, DbError> {
    let maybe_region = sqlx::query_scalar!("SELECT holiday_region FROM users WHERE id = $1", to_db(user))
      .fetch_optional(self.pool())
      .await?;
    Ok(maybe_region.flatten())
  }

  /// The holiday regions of those among `users` that picked one.
  pub async fn holiday_regions(&self, users: &[UserId]) -> Result<HashMap<UserId, String>, DbError> {
    let raw = to_db_all(users);
    let rows = sqlx::query!(
      r#"SELECT id, holiday_region AS "region!" FROM users
         WHERE id = ANY($1) AND holiday_region IS NOT NULL"#,
      &raw,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.id)?, row.region)))
      .collect()
  }

  /// Sets the user's holiday region (`None`: the instance's again). `true` if it changed.
  pub async fn set_holiday_region(&self, user: UserId, maybe_region: Option<&str>) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE users SET holiday_region = $2
         WHERE id = $1 AND holiday_region IS DISTINCT FROM $2"#,
      to_db(user),
      maybe_region,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }
}

pub(crate) fn user_from_parts(
  id: i64,
  maybe_username: Option<String>,
  display_name: String,
  kind: String,
  created_at: OffsetDateTime,
  maybe_organization: Option<String>,
  flags: AccountFlags,
) -> Result<User, DbError> {
  Ok(User {
    id: from_db(id)?,
    maybe_username,
    display_name,
    kind: UserKind::from(kind),
    created_at,
    is_deactivated: flags.is_deactivated,
    is_deleted: flags.is_deleted,
    maybe_organization,
  })
}

/// Whether an account is deactivated and whether it was deleted, as read from its row.
pub(crate) struct AccountFlags {
  pub(crate) is_deactivated: bool,
  pub(crate) is_deleted: bool,
}

/// Escapes `%`, `_` and `\` so user input is matched literally by `ILIKE`.
pub(crate) fn escape_like(input: &str) -> String {
  input
    .replace('\\', "\\\\")
    .replace('%', "\\%")
    .replace('_', "\\_")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn like_metacharacters_are_escaped() {
    assert_eq!(escape_like("100%_a\\b"), "100\\%\\_a\\\\b");
    assert_eq!(escape_like("alice"), "alice");
  }
}

/// A user known to the OIDC sync.
#[derive(Debug, Clone)]
pub struct OidcUserRow {
  pub id: UserId,
  pub subject: String,
  pub is_deactivated: bool,
  pub maybe_deactivation_reason: Option<String>,
  pub maybe_oidc_role: Option<String>,
  pub kind: UserKind,
}

impl Database {
  pub async fn find_user_by_oidc_subject(
    &self,
    issuer: &str,
    subject: &str,
  ) -> Result<Option<UserId>, DbError> {
    let id = sqlx::query_scalar!(
      "SELECT id FROM users WHERE oidc_issuer = $1 AND oidc_subject = $2",
      issuer,
      subject,
    )
    .fetch_optional(self.pool())
    .await?;
    id.map(from_db).transpose()
  }

  pub async fn list_oidc_users(&self, issuer: &str) -> Result<Vec<OidcUserRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, oidc_subject AS "subject!", deactivated_at IS NOT NULL AS "is_deactivated!",
                deactivation_reason, oidc_role, kind
         FROM users WHERE oidc_issuer = $1 AND oidc_subject IS NOT NULL ORDER BY id"#,
      issuer,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(OidcUserRow {
          id: from_db(row.id)?,
          subject: row.subject,
          is_deactivated: row.is_deactivated,
          maybe_deactivation_reason: row.deactivation_reason,
          maybe_oidc_role: row.oidc_role,
          kind: UserKind::from(row.kind),
        })
      })
      .collect()
  }

  /// Records a role change seen by the OIDC sync.
  pub async fn set_user_oidc_role(&self, user: UserId, role: &str, kind: &UserKind) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE users SET oidc_role = $2, kind = $3 WHERE id = $1",
      to_db(user),
      role,
      kind.as_str(),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }
}
