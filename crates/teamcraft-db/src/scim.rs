//! SCIM 2.0 provisioning (docs/provisioning.md §7): the clients and their token hashes, accounts and
//! groups as SCIM resources, and the window of the safety brake.
//!
//! An account deleted (tombstoned) by any path is no SCIM resource any more: every read leaves it
//! out, and a new link first drops the links of deleted accounts that would collide.

use std::collections::HashMap;
use std::time::Duration;

use teamcraft_proto::{ScimClientId, ScimGroupId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};

/// A SCIM client as the console lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimClientRow {
  pub id: ScimClientId,
  pub name: String,
  pub identity_attribute: String,
  pub created_at: OffsetDateTime,
  pub token_rotated_at: OffsetDateTime,
  pub maybe_last_used_at: Option<OffsetDateTime>,
  pub user_count: i64,
  pub group_count: i64,
}

/// What authenticating a request needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimClientCredentials {
  pub id: ScimClientId,
  pub token_sha256: Vec<u8>,
  pub identity_attribute: String,
}

/// A new SCIM client.
#[derive(Debug, Clone)]
pub struct NewScimClient<'a> {
  pub id: ScimClientId,
  pub name: &'a str,
  pub token_sha256: &'a [u8],
  pub identity_attribute: &'a str,
  pub created_by: UserId,
}

/// How storing a new client went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScimClientCreation {
  Created,
  /// The instance has `max` clients already.
  TooMany,
}

/// An account as a SCIM resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimUserRow {
  pub user: UserId,
  pub user_name: String,
  pub maybe_external_id: Option<String>,
  pub maybe_given_name: Option<String>,
  pub maybe_family_name: Option<String>,
  pub maybe_display_name: Option<String>,
  pub maybe_email: Option<String>,
  pub is_active: bool,
  pub version: i64,
  pub created_at: OffsetDateTime,
  pub updated_at: OffsetDateTime,
}

/// What a SCIM write sets on an account and its resource.
#[derive(Debug, Clone)]
pub struct ScimUserWrite<'a> {
  pub user_name: &'a str,
  pub maybe_external_id: Option<&'a str>,
  pub maybe_given_name: Option<&'a str>,
  pub maybe_family_name: Option<&'a str>,
  pub maybe_display_name: Option<&'a str>,
  /// The name the account shows while it never signed in.
  pub shown_name: &'a str,
  pub maybe_email: Option<&'a str>,
}

/// How a user lookup narrows the client's resources.
#[derive(Debug, Clone, Copy)]
pub enum ScimUserLookup<'a> {
  UserName(&'a str),
  ExternalId(&'a str),
  /// All of them, at most this many (a filter no index helps with).
  All {
    limit: i64,
  },
}

/// How linking an account went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScimLink {
  Linked,
  /// This client has another account with the user name.
  UserNameTaken,
  /// The account is a resource of another client.
  LinkedElsewhere,
  /// The account was a resource of this client already (a retried create).
  AlreadyLinked,
}

/// How a write to a resource went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScimWrite {
  /// Written; the new version.
  Written(i64),
  NotFound,
  /// `If-Match` named another version.
  VersionMismatch,
  /// The new user name or group display name belongs to another resource of the client.
  NameTaken,
}

/// A group as a SCIM resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimGroupRow {
  pub id: ScimGroupId,
  pub display_name: String,
  pub maybe_external_id: Option<String>,
  pub version: i64,
  pub created_at: OffsetDateTime,
  pub updated_at: OffsetDateTime,
}

/// How a group lookup narrows the client's groups.
#[derive(Debug, Clone, Copy)]
pub enum ScimGroupLookup<'a> {
  DisplayName(&'a str),
  ExternalId(&'a str),
  All { limit: i64 },
}

/// One change of a group, applied in order in one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScimGroupWrite {
  DisplayName(String),
  ExternalId(Option<String>),
  AddMembers(Vec<UserId>),
  RemoveMembers(Vec<UserId>),
  ReplaceMembers(Vec<UserId>),
  RemoveAllMembers,
}

/// How changing a group went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScimGroupUpdate {
  /// Written: the new version, the accounts that became members, and whether the name changed.
  Written {
    version: i64,
    added: Vec<UserId>,
    is_renamed: bool,
  },
  NotFound,
  VersionMismatch,
  NameTaken,
  /// Members that are no account of this client (unknown, deleted, or another client's).
  UnknownMembers(Vec<UserId>),
  /// The group would have more members than allowed.
  TooManyMembers,
}

/// How making a group went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScimGroupCreation {
  Created,
  NameTaken,
  UnknownMembers(Vec<UserId>),
  TooManyMembers,
}

impl Database {
  /// Stores a client unless the instance has `max` already (checked under a lock, so two console
  /// requests can't both pass).
  pub async fn create_scim_client(
    &self,
    client: &NewScimClient<'_>,
    max: usize,
  ) -> Result<ScimClientCreation, DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!("LOCK TABLE scim_clients IN SHARE ROW EXCLUSIVE MODE")
      .execute(&mut *tx)
      .await?;
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM scim_clients"#)
      .fetch_one(&mut *tx)
      .await?;
    if usize::try_from(count).unwrap_or(usize::MAX) >= max {
      return Ok(ScimClientCreation::TooMany);
    }
    sqlx::query!(
      "INSERT INTO scim_clients (id, name, token_sha256, identity_attribute, created_by)
       VALUES ($1, $2, $3, $4, $5)",
      to_db(client.id),
      client.name,
      client.token_sha256,
      client.identity_attribute,
      to_db(client.created_by),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ScimClientCreation::Created)
  }

  /// Every client, oldest first, with how many accounts and groups it provisioned.
  pub async fn list_scim_clients(&self) -> Result<Vec<ScimClientRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT c.id, c.name, c.identity_attribute, c.created_at, c.token_rotated_at, c.last_used_at,
                (SELECT count(*) FROM scim_users s JOIN users u ON u.id = s.user_id
                 WHERE s.client_id = c.id AND u.deleted_at IS NULL) AS "user_count!",
                (SELECT count(*) FROM scim_groups g WHERE g.client_id = c.id) AS "group_count!"
         FROM scim_clients c ORDER BY c.created_at, c.id"#
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ScimClientRow {
          id: from_db(row.id)?,
          name: row.name,
          identity_attribute: row.identity_attribute,
          created_at: row.created_at,
          token_rotated_at: row.token_rotated_at,
          maybe_last_used_at: row.last_used_at,
          user_count: row.user_count,
          group_count: row.group_count,
        })
      })
      .collect()
  }

  /// The client's token hash, to check a request's token against.
  pub async fn scim_client_credentials(
    &self,
    client: ScimClientId,
  ) -> Result<Option<ScimClientCredentials>, DbError> {
    let row = sqlx::query!(
      "SELECT id, token_sha256, identity_attribute FROM scim_clients WHERE id = $1",
      to_db(client),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(ScimClientCredentials {
          id: from_db(row.id)?,
          token_sha256: row.token_sha256,
          identity_attribute: row.identity_attribute,
        })
      })
      .transpose()
  }

  /// Replaces the client's token at once; `false` if there is no such client.
  pub async fn rotate_scim_client_token(
    &self,
    client: ScimClientId,
    token_sha256: &[u8],
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE scim_clients SET token_sha256 = $2, token_rotated_at = now() WHERE id = $1",
      to_db(client),
      token_sha256,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// Removes the client and its groups; the accounts it made stay, as accounts without a SCIM
  /// client. `false` if there was none.
  pub async fn delete_scim_client(&self, client: ScimClientId) -> Result<bool, DbError> {
    let result = sqlx::query!("DELETE FROM scim_clients WHERE id = $1", to_db(client))
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected() > 0)
  }

  /// A request with the client's token arrived: noted at most once a minute, so requests don't
  /// each write.
  pub async fn note_scim_client_used(&self, client: ScimClientId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE scim_clients SET last_used_at = now()
       WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
      to_db(client),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// One account of the client.
  pub async fn scim_user(&self, client: ScimClientId, user: UserId) -> Result<Option<ScimUserRow>, DbError> {
    let mut rows = self
      .scim_users_where(client, Some(user), None, None, 1, 0)
      .await?;
    Ok(rows.pop())
  }

  /// The client's accounts a lookup names, ordered by account.
  pub async fn find_scim_users(
    &self,
    client: ScimClientId,
    lookup: ScimUserLookup<'_>,
  ) -> Result<Vec<ScimUserRow>, DbError> {
    match lookup {
      ScimUserLookup::UserName(name) => self.scim_users_where(client, None, Some(name), None, 2, 0).await,
      ScimUserLookup::ExternalId(id) => self.scim_users_where(client, None, None, Some(id), 1000, 0).await,
      ScimUserLookup::All { limit } => self.scim_users_where(client, None, None, None, limit, 0).await,
    }
  }

  /// A page of the client's accounts and how many there are.
  pub async fn scim_users_page(
    &self,
    client: ScimClientId,
    offset: i64,
    limit: i64,
  ) -> Result<(Vec<ScimUserRow>, i64), DbError> {
    let total = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM scim_users s JOIN users u ON u.id = s.user_id
         WHERE s.client_id = $1 AND u.deleted_at IS NULL"#,
      to_db(client),
    )
    .fetch_one(self.pool())
    .await?;
    let rows = self
      .scim_users_where(client, None, None, None, limit, offset)
      .await?;
    Ok((rows, total))
  }

  async fn scim_users_where(
    &self,
    client: ScimClientId,
    maybe_user: Option<UserId>,
    maybe_user_name: Option<&str>,
    maybe_external_id: Option<&str>,
    limit: i64,
    offset: i64,
  ) -> Result<Vec<ScimUserRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT s.user_id, s.user_name, s.external_id, s.given_name, s.family_name,
                s.display_name, s.email,
                u.deactivated_at IS NULL AS "is_active!", s.version, s.created_at, s.updated_at
         FROM scim_users s JOIN users u ON u.id = s.user_id
         WHERE s.client_id = $1 AND u.deleted_at IS NULL
           AND ($2::bigint IS NULL OR s.user_id = $2)
           AND ($3::text IS NULL OR lower(s.user_name) = lower($3))
           AND ($4::text IS NULL OR s.external_id = $4)
         ORDER BY s.user_id
         LIMIT $5 OFFSET $6"#,
      to_db(client),
      maybe_user.map(to_db),
      maybe_user_name,
      maybe_external_id,
      limit,
      offset,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ScimUserRow {
          user: from_db(row.user_id)?,
          user_name: row.user_name,
          maybe_external_id: row.external_id,
          maybe_given_name: row.given_name,
          maybe_family_name: row.family_name,
          maybe_display_name: row.display_name,
          maybe_email: row.email,
          is_active: row.is_active,
          version: row.version,
          created_at: row.created_at,
          updated_at: row.updated_at,
        })
      })
      .collect()
  }

  /// Makes `user` a resource of `client` and writes its attributes to the account. A link of a
  /// deleted account with the same user name goes first.
  pub async fn link_scim_user(
    &self,
    client: ScimClientId,
    user: UserId,
    write: &ScimUserWrite<'_>,
  ) -> Result<ScimLink, DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      "DELETE FROM scim_users s USING users u
       WHERE u.id = s.user_id AND u.deleted_at IS NOT NULL
         AND (s.user_id = $2 OR (s.client_id = $1 AND lower(s.user_name) = lower($3)))",
      to_db(client),
      to_db(user),
      write.user_name,
    )
    .execute(&mut *tx)
    .await?;
    let maybe_owner = sqlx::query_scalar!("SELECT client_id FROM scim_users WHERE user_id = $1", to_db(user))
      .fetch_optional(&mut *tx)
      .await?;
    match maybe_owner {
      Some(owner) if owner == to_db(client) => return Ok(ScimLink::AlreadyLinked),
      Some(_) => return Ok(ScimLink::LinkedElsewhere),
      None => {},
    }
    let inserted = sqlx::query!(
      "INSERT INTO scim_users (user_id, client_id, user_name, external_id, given_name, family_name, display_name, email)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
       ON CONFLICT DO NOTHING",
      to_db(user),
      to_db(client),
      write.user_name,
      write.maybe_external_id,
      write.maybe_given_name,
      write.maybe_family_name,
      write.maybe_display_name,
      write.maybe_email,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
      return Ok(ScimLink::UserNameTaken);
    }
    write_account(&mut tx, user, write).await?;
    tx.commit().await?;
    Ok(ScimLink::Linked)
  }

  /// Replaces a resource's attributes, if it is still at `maybe_expected_version` (`If-Match`).
  pub async fn update_scim_user(
    &self,
    client: ScimClientId,
    user: UserId,
    write: &ScimUserWrite<'_>,
    maybe_expected_version: Option<i64>,
  ) -> Result<ScimWrite, DbError> {
    let mut tx = self.pool().begin().await?;
    let maybe_version = sqlx::query_scalar!(
      "SELECT s.version FROM scim_users s JOIN users u ON u.id = s.user_id
       WHERE s.client_id = $1 AND s.user_id = $2 AND u.deleted_at IS NULL
       FOR UPDATE OF s",
      to_db(client),
      to_db(user),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(version) = maybe_version else {
      return Ok(ScimWrite::NotFound);
    };
    if maybe_expected_version.is_some_and(|expected| expected != version) {
      return Ok(ScimWrite::VersionMismatch);
    }
    let is_taken = sqlx::query_scalar!(
      r#"SELECT EXISTS (SELECT 1 FROM scim_users s JOIN users u ON u.id = s.user_id
                        WHERE s.client_id = $1 AND s.user_id <> $2 AND u.deleted_at IS NULL
                          AND lower(s.user_name) = lower($3)) AS "is_taken!""#,
      to_db(client),
      to_db(user),
      write.user_name,
    )
    .fetch_one(&mut *tx)
    .await?;
    if is_taken {
      return Ok(ScimWrite::NameTaken);
    }
    // A deleted account's link with this name would collide with the unique index.
    sqlx::query!(
      "DELETE FROM scim_users s USING users u
       WHERE u.id = s.user_id AND u.deleted_at IS NOT NULL AND s.client_id = $1
         AND lower(s.user_name) = lower($2)",
      to_db(client),
      write.user_name,
    )
    .execute(&mut *tx)
    .await?;
    let new_version = sqlx::query_scalar!(
      "UPDATE scim_users
       SET user_name = $2, external_id = $3, given_name = $4, family_name = $5, display_name = $6,
           email = $7, version = version + 1, updated_at = now()
       WHERE user_id = $1 RETURNING version",
      to_db(user),
      write.user_name,
      write.maybe_external_id,
      write.maybe_given_name,
      write.maybe_family_name,
      write.maybe_display_name,
      write.maybe_email,
    )
    .fetch_one(&mut *tx)
    .await?;
    write_account(&mut tx, user, write).await?;
    tx.commit().await?;
    Ok(ScimWrite::Written(new_version))
  }

  /// Raises a resource's version after a change of the account outside its attributes (`active`).
  pub async fn touch_scim_user(&self, client: ScimClientId, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE scim_users SET version = version + 1, updated_at = now() WHERE client_id = $1 AND user_id = $2",
      to_db(client),
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// The account is no resource of the client any more (a SCIM DELETE); its group memberships go
  /// too. `false` if it wasn't one.
  pub async fn unlink_scim_user(&self, client: ScimClientId, user: UserId) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let removed = sqlx::query!(
      "DELETE FROM scim_users WHERE client_id = $1 AND user_id = $2",
      to_db(client),
      to_db(user),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    sqlx::query!(
      "DELETE FROM scim_group_members m USING scim_groups g
       WHERE g.id = m.group_id AND g.client_id = $1 AND m.user_id = $2",
      to_db(client),
      to_db(user),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(removed > 0)
  }

  /// The client's groups each of `users` is in, by account.
  pub async fn scim_groups_of_users(
    &self,
    client: ScimClientId,
    users: &[UserId],
  ) -> Result<HashMap<UserId, Vec<(ScimGroupId, String)>>, DbError> {
    let rows = sqlx::query!(
      "SELECT m.user_id, g.id, g.display_name FROM scim_group_members m JOIN scim_groups g ON g.id = m.group_id
       WHERE g.client_id = $1 AND m.user_id = ANY($2) ORDER BY g.display_name, g.id",
      to_db(client),
      &to_db_all(users),
    )
    .fetch_all(self.pool())
    .await?;
    let mut groups: HashMap<UserId, Vec<(ScimGroupId, String)>> = HashMap::new();
    for row in rows {
      groups
        .entry(from_db(row.user_id)?)
        .or_default()
        .push((from_db(row.id)?, row.display_name));
    }
    Ok(groups)
  }

  /// Makes a group with its members, which must be accounts of the client.
  pub async fn create_scim_group(
    &self,
    client: ScimClientId,
    group: &ScimGroupRow,
    members: &[UserId],
    max_members: usize,
  ) -> Result<ScimGroupCreation, DbError> {
    if members.len() > max_members {
      return Ok(ScimGroupCreation::TooManyMembers);
    }
    let mut tx = self.pool().begin().await?;
    let unknown = unknown_members(&mut tx, client, members).await?;
    if !unknown.is_empty() {
      return Ok(ScimGroupCreation::UnknownMembers(unknown));
    }
    let inserted = sqlx::query!(
      "INSERT INTO scim_groups (id, client_id, display_name, external_id) VALUES ($1, $2, $3, $4)
       ON CONFLICT DO NOTHING",
      to_db(group.id),
      to_db(client),
      group.display_name,
      group.maybe_external_id,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
      return Ok(ScimGroupCreation::NameTaken);
    }
    add_members(&mut tx, group.id, members).await?;
    tx.commit().await?;
    Ok(ScimGroupCreation::Created)
  }

  pub async fn scim_group(
    &self,
    client: ScimClientId,
    group: ScimGroupId,
  ) -> Result<Option<ScimGroupRow>, DbError> {
    let mut rows = self
      .scim_groups_where(client, Some(group), None, None, 1, 0)
      .await?;
    Ok(rows.pop())
  }

  pub async fn find_scim_groups(
    &self,
    client: ScimClientId,
    lookup: ScimGroupLookup<'_>,
  ) -> Result<Vec<ScimGroupRow>, DbError> {
    match lookup {
      ScimGroupLookup::DisplayName(name) => {
        self.scim_groups_where(client, None, Some(name), None, 2, 0).await
      },
      ScimGroupLookup::ExternalId(id) => {
        self
          .scim_groups_where(client, None, None, Some(id), 1000, 0)
          .await
      },
      ScimGroupLookup::All { limit } => self.scim_groups_where(client, None, None, None, limit, 0).await,
    }
  }

  pub async fn scim_groups_page(
    &self,
    client: ScimClientId,
    offset: i64,
    limit: i64,
  ) -> Result<(Vec<ScimGroupRow>, i64), DbError> {
    let total = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM scim_groups WHERE client_id = $1"#,
      to_db(client),
    )
    .fetch_one(self.pool())
    .await?;
    let rows = self
      .scim_groups_where(client, None, None, None, limit, offset)
      .await?;
    Ok((rows, total))
  }

  async fn scim_groups_where(
    &self,
    client: ScimClientId,
    maybe_group: Option<ScimGroupId>,
    maybe_display_name: Option<&str>,
    maybe_external_id: Option<&str>,
    limit: i64,
    offset: i64,
  ) -> Result<Vec<ScimGroupRow>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, display_name, external_id, version, created_at, updated_at FROM scim_groups
       WHERE client_id = $1
         AND ($2::bigint IS NULL OR id = $2)
         AND ($3::text IS NULL OR lower(display_name) = lower($3))
         AND ($4::text IS NULL OR external_id = $4)
       ORDER BY id LIMIT $5 OFFSET $6",
      to_db(client),
      maybe_group.map(to_db),
      maybe_display_name,
      maybe_external_id,
      limit,
      offset,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ScimGroupRow {
          id: from_db(row.id)?,
          display_name: row.display_name,
          maybe_external_id: row.external_id,
          version: row.version,
          created_at: row.created_at,
          updated_at: row.updated_at,
        })
      })
      .collect()
  }

  /// The members of groups, with their display names, by group; deleted accounts left out.
  pub async fn scim_group_members(
    &self,
    groups: &[ScimGroupId],
  ) -> Result<HashMap<ScimGroupId, Vec<(UserId, String)>>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.group_id, m.user_id, COALESCE(u.idp_display_name, u.display_name) AS "display_name!"
         FROM scim_group_members m JOIN users u ON u.id = m.user_id
         WHERE m.group_id = ANY($1) AND u.deleted_at IS NULL ORDER BY m.group_id, m.user_id"#,
      &to_db_all(groups),
    )
    .fetch_all(self.pool())
    .await?;
    let mut members: HashMap<ScimGroupId, Vec<(UserId, String)>> = HashMap::new();
    for row in rows {
      members
        .entry(from_db(row.group_id)?)
        .or_default()
        .push((from_db(row.user_id)?, row.display_name));
    }
    Ok(members)
  }

  /// The active members of every SCIM group named `display_name` (any client, case ignored): who
  /// a group mapping rule for that name applies to.
  pub async fn scim_group_members_named(&self, display_name: &str) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      "SELECT DISTINCT m.user_id FROM scim_group_members m
       JOIN scim_groups g ON g.id = m.group_id JOIN users u ON u.id = m.user_id
       WHERE lower(g.display_name) = lower($1) AND u.deactivated_at IS NULL AND u.deleted_at IS NULL
       ORDER BY m.user_id",
      display_name,
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// Applies `changes` to a group in one transaction, if it is still at `maybe_expected_version`.
  pub async fn update_scim_group(
    &self,
    client: ScimClientId,
    group: ScimGroupId,
    changes: &[ScimGroupWrite],
    maybe_expected_version: Option<i64>,
    max_members: usize,
  ) -> Result<ScimGroupUpdate, DbError> {
    let mut tx = self.pool().begin().await?;
    let maybe_row = sqlx::query!(
      "SELECT version, display_name FROM scim_groups WHERE client_id = $1 AND id = $2 FOR UPDATE",
      to_db(client),
      to_db(group),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(row) = maybe_row else {
      return Ok(ScimGroupUpdate::NotFound);
    };
    if maybe_expected_version.is_some_and(|expected| expected != row.version) {
      return Ok(ScimGroupUpdate::VersionMismatch);
    }
    let named: Vec<UserId> = changes
      .iter()
      .flat_map(|change| match change {
        ScimGroupWrite::AddMembers(ids) | ScimGroupWrite::ReplaceMembers(ids) => ids.clone(),
        _ => Vec::new(),
      })
      .collect();
    let unknown = unknown_members(&mut tx, client, &named).await?;
    if !unknown.is_empty() {
      return Ok(ScimGroupUpdate::UnknownMembers(unknown));
    }
    let before: Vec<i64> = sqlx::query_scalar!(
      "SELECT user_id FROM scim_group_members WHERE group_id = $1",
      to_db(group)
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut is_renamed = false;
    for change in changes {
      match change {
        ScimGroupWrite::DisplayName(name) => {
          let is_taken = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM scim_groups WHERE client_id = $1 AND id <> $2
                              AND lower(display_name) = lower($3)) AS "is_taken!""#,
            to_db(client),
            to_db(group),
            name,
          )
          .fetch_one(&mut *tx)
          .await?;
          if is_taken {
            return Ok(ScimGroupUpdate::NameTaken);
          }
          is_renamed |= !name.eq_ignore_ascii_case(&row.display_name);
          sqlx::query!(
            "UPDATE scim_groups SET display_name = $2 WHERE id = $1",
            to_db(group),
            name
          )
          .execute(&mut *tx)
          .await?;
        },
        ScimGroupWrite::ExternalId(maybe_id) => {
          sqlx::query!(
            "UPDATE scim_groups SET external_id = $2 WHERE id = $1",
            to_db(group),
            maybe_id.as_deref()
          )
          .execute(&mut *tx)
          .await?;
        },
        ScimGroupWrite::AddMembers(ids) => add_members(&mut tx, group, ids).await?,
        ScimGroupWrite::RemoveMembers(ids) => {
          sqlx::query!(
            "DELETE FROM scim_group_members WHERE group_id = $1 AND user_id = ANY($2)",
            to_db(group),
            &to_db_all(ids),
          )
          .execute(&mut *tx)
          .await?;
        },
        ScimGroupWrite::ReplaceMembers(ids) => {
          sqlx::query!(
            "DELETE FROM scim_group_members WHERE group_id = $1 AND NOT (user_id = ANY($2))",
            to_db(group),
            &to_db_all(ids),
          )
          .execute(&mut *tx)
          .await?;
          add_members(&mut tx, group, ids).await?;
        },
        ScimGroupWrite::RemoveAllMembers => {
          sqlx::query!("DELETE FROM scim_group_members WHERE group_id = $1", to_db(group))
            .execute(&mut *tx)
            .await?;
        },
      }
    }
    let after: Vec<i64> = sqlx::query_scalar!(
      "SELECT user_id FROM scim_group_members WHERE group_id = $1",
      to_db(group)
    )
    .fetch_all(&mut *tx)
    .await?;
    if after.len() > max_members {
      return Ok(ScimGroupUpdate::TooManyMembers);
    }
    let version = sqlx::query_scalar!(
      "UPDATE scim_groups SET version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
      to_db(group),
    )
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    let added = after
      .into_iter()
      .filter(|id| !before.contains(id))
      .map(from_db)
      .collect::<Result<Vec<UserId>, _>>()?;
    Ok(ScimGroupUpdate::Written {
      version,
      added,
      is_renamed,
    })
  }

  /// Removes a group; `false` if there was none.
  pub async fn delete_scim_group(&self, client: ScimClientId, group: ScimGroupId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "DELETE FROM scim_groups WHERE client_id = $1 AND id = $2",
      to_db(client),
      to_db(group),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// Takes a place in the safety brake's window for deactivating or removing `user`, if
  /// `may_proceed(recent, active)` allows it: `recent` is how many SCIM deprovisions happened in
  /// the last `window`, `active` how many active accounts of `issuer` there are. Serialized by an
  /// advisory lock, so concurrent requests can't both take the last place. `false`: hold it.
  pub async fn reserve_scim_deprovision(
    &self,
    user: UserId,
    issuer: &str,
    window: Duration,
    may_proceed: impl FnOnce(u64, u64) -> bool,
  ) -> Result<bool, DbError> {
    let window_secs = f64::from(u32::try_from(window.as_secs()).unwrap_or(u32::MAX));
    let mut tx = self.pool().begin().await?;
    // Any constant: one lock for the brake of every SCIM client.
    sqlx::query!("SELECT pg_advisory_xact_lock(7_631_000_001)")
      .execute(&mut *tx)
      .await?;
    sqlx::query!(
      "DELETE FROM scim_deprovisions WHERE at < now() - make_interval(secs => $1)",
      window_secs,
    )
    .execute(&mut *tx)
    .await?;
    let recent = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM scim_deprovisions"#)
      .fetch_one(&mut *tx)
      .await?;
    let active = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM users
         WHERE oidc_issuer = $1 AND deactivated_at IS NULL AND deleted_at IS NULL"#,
      issuer,
    )
    .fetch_one(&mut *tx)
    .await?;
    let to_count = |value: i64| u64::try_from(value).unwrap_or(0);
    if !may_proceed(to_count(recent), to_count(active)) {
      return Ok(false);
    }
    sqlx::query!("INSERT INTO scim_deprovisions (user_id) VALUES ($1)", to_db(user))
      .execute(&mut *tx)
      .await?;
    tx.commit().await?;
    Ok(true)
  }

  /// Puts a SCIM deactivation on the safety brake's list for an admin to confirm. Idempotent.
  pub async fn hold_scim_deactivation(&self, user: UserId, cause: &str) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    let added = sqlx::query!(
      "INSERT INTO oidc_sync_held_deactivations (user_id, cause, source) VALUES ($1, $2, 'scim')
       ON CONFLICT (user_id) DO NOTHING",
      to_db(user),
      cause,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if added > 0 {
      crate::oidc_sync::bump_held_revision(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(())
  }

  /// Takes a SCIM hold off the list (the provider enabled the person again, or it was applied).
  pub async fn release_scim_hold(&self, user: UserId) -> Result<bool, DbError> {
    let mut tx = self.pool().begin().await?;
    let removed = sqlx::query!(
      "DELETE FROM oidc_sync_held_deactivations WHERE user_id = $1 AND source = 'scim'",
      to_db(user),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if removed > 0 {
      crate::oidc_sync::bump_held_revision(&mut tx).await?;
    }
    tx.commit().await?;
    Ok(removed > 0)
  }

  /// SCIM holds an admin confirmed, to apply now.
  pub async fn confirmed_scim_holds(&self) -> Result<Vec<(UserId, String)>, DbError> {
    let rows = sqlx::query!(
      "SELECT user_id, cause FROM oidc_sync_held_deactivations
       WHERE source = 'scim' AND confirmed_at IS NOT NULL ORDER BY held_at, user_id"
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.user_id)?, row.cause)))
      .collect()
  }
}

/// Of `members`, those that are no live account of `client`.
async fn unknown_members(
  tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
  client: ScimClientId,
  members: &[UserId],
) -> Result<Vec<UserId>, DbError> {
  if members.is_empty() {
    return Ok(Vec::new());
  }
  let known: Vec<i64> = sqlx::query_scalar!(
    "SELECT s.user_id FROM scim_users s JOIN users u ON u.id = s.user_id
     WHERE s.client_id = $1 AND s.user_id = ANY($2) AND u.deleted_at IS NULL",
    to_db(client),
    &to_db_all(members),
  )
  .fetch_all(&mut **tx)
  .await?;
  Ok(
    members
      .iter()
      .copied()
      .filter(|member| !known.contains(&to_db(*member)))
      .collect(),
  )
}

async fn add_members(
  tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
  group: ScimGroupId,
  members: &[UserId],
) -> Result<(), DbError> {
  sqlx::query!(
    "INSERT INTO scim_group_members (group_id, user_id) SELECT $1, UNNEST($2::bigint[])
     ON CONFLICT DO NOTHING",
    to_db(group),
    &to_db_all(members),
  )
  .execute(&mut **tx)
  .await?;
  Ok(())
}

/// Writes the provider's name, user name and address to an account that never signed in, like the
/// sync's pre-creation; once it signed in, its token is their source and SCIM leaves them alone, so
/// the two never take turns. A name the user chose (`display_name_override`) stays.
async fn write_account(
  tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
  user: UserId,
  write: &ScimUserWrite<'_>,
) -> Result<(), DbError> {
  sqlx::query!(
    "UPDATE users SET username = $2, email = $3, idp_display_name = $4,
                      display_name = COALESCE(display_name_override, $4)
     WHERE id = $1 AND awaiting_first_sign_in_since IS NOT NULL",
    to_db(user),
    write.user_name,
    write.maybe_email,
    write.shown_name,
  )
  .execute(&mut **tx)
  .await?;
  Ok(())
}

/// What deprovisioning an account through SCIM needs to know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScimAccountState {
  pub is_deactivated: bool,
  pub maybe_deactivation_reason: Option<String>,
  pub maybe_oidc_role: Option<String>,
  pub is_awaiting_first_sign_in: bool,
}

impl Database {
  /// The state of a live (not deleted) account.
  pub async fn scim_account_state(&self, user: UserId) -> Result<Option<ScimAccountState>, DbError> {
    let row = sqlx::query!(
      r#"SELECT deactivated_at IS NOT NULL AS "is_deactivated!", deactivation_reason, oidc_role,
                awaiting_first_sign_in_since IS NOT NULL AS "is_awaiting_first_sign_in!"
         FROM users WHERE id = $1 AND deleted_at IS NULL"#,
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(row.map(|row| ScimAccountState {
      is_deactivated: row.is_deactivated,
      maybe_deactivation_reason: row.deactivation_reason,
      maybe_oidc_role: row.oidc_role,
      is_awaiting_first_sign_in: row.is_awaiting_first_sign_in,
    }))
  }

  /// How many members each group has (deleted accounts left out).
  pub async fn scim_group_member_counts(
    &self,
    groups: &[ScimGroupId],
  ) -> Result<HashMap<ScimGroupId, i64>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.group_id, count(*) AS "count!" FROM scim_group_members m JOIN users u ON u.id = m.user_id
         WHERE m.group_id = ANY($1) AND u.deleted_at IS NULL GROUP BY m.group_id"#,
      &to_db_all(groups),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.group_id)?, row.count)))
      .collect()
  }
}
