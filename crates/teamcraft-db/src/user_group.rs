//! User groups (`@devteam`).

use std::collections::HashSet;

use teamcraft_proto::{UserGroupId, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserGroup {
  pub id: UserGroupId,
  pub handle: String,
  pub name: String,
  pub maybe_description: Option<String>,
  pub maybe_external_id: Option<String>,
  pub member_count: u32,
}

#[derive(Debug, Clone)]
pub struct NewUserGroup<'a> {
  pub id: UserGroupId,
  pub handle: &'a str,
  pub name: &'a str,
  pub maybe_description: Option<&'a str>,
  pub maybe_external_id: Option<&'a str>,
  pub maybe_created_by: Option<UserId>,
}

/// Why a group could not be created.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupCreateOutcome {
  Created,
  /// Another group or a username already uses the handle.
  HandleTaken,
}

impl Database {
  pub async fn create_user_group(&self, new: &NewUserGroup<'_>) -> Result<GroupCreateOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    let is_username = sqlx::query!(
      r#"SELECT EXISTS (SELECT 1 FROM users WHERE lower(username) = $1) AS "exists!""#,
      new.handle,
    )
    .fetch_one(&mut *tx)
    .await?
    .exists;
    if is_username {
      return Ok(GroupCreateOutcome::HandleTaken);
    }
    let inserted = sqlx::query!(
      r#"INSERT INTO user_groups (id, handle, name, description, external_ref, created_by)
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT DO NOTHING"#,
      to_db(new.id),
      new.handle,
      new.name,
      new.maybe_description,
      new.maybe_external_id,
      new.maybe_created_by.map(to_db),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if inserted == 0 {
      return Ok(GroupCreateOutcome::HandleTaken);
    }
    tx.commit().await?;
    Ok(GroupCreateOutcome::Created)
  }

  pub async fn get_user_group(&self, id: UserGroupId) -> Result<Option<UserGroup>, DbError> {
    let row = sqlx::query!(
      r#"SELECT g.id, g.handle, g.name, g.description, g.external_ref,
                (SELECT count(*) FROM user_group_members m WHERE m.group_id = g.id) AS "member_count!"
         FROM user_groups g WHERE g.id = $1"#,
      to_db(id),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(UserGroup {
          id: from_db(row.id)?,
          handle: row.handle,
          name: row.name,
          maybe_description: row.description,
          maybe_external_id: row.external_ref,
          member_count: u32::try_from(row.member_count).unwrap_or(u32::MAX),
        })
      })
      .transpose()
  }

  pub async fn list_user_groups(&self) -> Result<Vec<UserGroup>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT g.id, g.handle, g.name, g.description, g.external_ref,
                (SELECT count(*) FROM user_group_members m WHERE m.group_id = g.id) AS "member_count!"
         FROM user_groups g ORDER BY g.handle"#,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(UserGroup {
          id: from_db(row.id)?,
          handle: row.handle,
          name: row.name,
          maybe_description: row.description,
          maybe_external_id: row.external_ref,
          member_count: u32::try_from(row.member_count).unwrap_or(u32::MAX),
        })
      })
      .collect()
  }

  pub async fn update_user_group(
    &self,
    id: UserGroupId,
    maybe_name: Option<&str>,
    maybe_description: Option<&str>,
    clears_description: bool,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE user_groups
         SET name = COALESCE($2, name),
             description = CASE WHEN $4 THEN NULL ELSE COALESCE($3, description) END
         WHERE id = $1"#,
      to_db(id),
      maybe_name,
      maybe_description,
      clears_description,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  pub async fn delete_user_group(&self, id: UserGroupId) -> Result<bool, DbError> {
    let result = sqlx::query!("DELETE FROM user_groups WHERE id = $1", to_db(id))
      .execute(self.pool())
      .await?;
    Ok(result.rows_affected() == 1)
  }

  pub async fn list_user_group_member_ids(&self, id: UserGroupId) -> Result<Vec<UserId>, DbError> {
    let rows = sqlx::query!(
      "SELECT user_id FROM user_group_members WHERE group_id = $1 ORDER BY user_id",
      to_db(id)
    )
    .fetch_all(self.pool())
    .await?;
    rows.into_iter().map(|row| from_db(row.user_id)).collect()
  }

  /// `true` if the user was added, `false` if they already were a member.
  pub async fn add_user_group_member(&self, id: UserGroupId, user: UserId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "INSERT INTO user_group_members (group_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
      to_db(id),
      to_db(user)
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  pub async fn remove_user_group_member(&self, id: UserGroupId, user: UserId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "DELETE FROM user_group_members WHERE group_id = $1 AND user_id = $2",
      to_db(id),
      to_db(user)
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// The group the identity provider calls `external_id`.
  pub async fn find_user_group_by_external_id(
    &self,
    external_id: &str,
  ) -> Result<Option<UserGroup>, DbError> {
    let row = sqlx::query!("SELECT id FROM user_groups WHERE external_ref = $1", external_id)
      .fetch_optional(self.pool())
      .await?;
    match row {
      Some(row) => self.get_user_group(from_db(row.id)?).await,
      None => Ok(None),
    }
  }

  /// Makes the members exactly `users`. Returns whether anything changed.
  pub async fn set_user_group_members(&self, id: UserGroupId, users: &[UserId]) -> Result<bool, DbError> {
    let wanted: HashSet<UserId> = users.iter().copied().collect();
    let current: HashSet<UserId> = self.list_user_group_member_ids(id).await?.into_iter().collect();
    if wanted == current {
      return Ok(false);
    }
    let mut tx = self.pool().begin().await?;
    let stale: Vec<UserId> = current.difference(&wanted).copied().collect();
    sqlx::query!(
      "DELETE FROM user_group_members WHERE group_id = $1 AND user_id = ANY($2)",
      to_db(id),
      &to_db_all(&stale),
    )
    .execute(&mut *tx)
    .await?;
    let added: Vec<UserId> = wanted.difference(&current).copied().collect();
    sqlx::query!(
      r#"INSERT INTO user_group_members (group_id, user_id)
         SELECT $1, u FROM unnest($2::bigint[]) AS u ON CONFLICT DO NOTHING"#,
      to_db(id),
      &to_db_all(&added),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
  }

  /// Ids of the groups the identity provider owns.
  pub async fn list_synced_group_external_ids(&self) -> Result<Vec<(UserGroupId, String)>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, external_ref AS "external_ref!" FROM user_groups WHERE external_ref IS NOT NULL"#
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.id)?, row.external_ref)))
      .collect()
  }

  /// The users with these identity provider subjects.
  pub async fn find_users_by_subjects(
    &self,
    issuer: &str,
    subjects: &[String],
  ) -> Result<Vec<UserId>, DbError> {
    let rows = sqlx::query!(
      "SELECT id FROM users WHERE oidc_issuer = $1 AND oidc_subject = ANY($2) AND deactivated_at IS NULL AND kind = 'member'",
      issuer,
      subjects,
    )
    .fetch_all(self.pool())
    .await?;
    rows.into_iter().map(|row| from_db(row.id)).collect()
  }
}
