use std::collections::HashMap;

use teamcraft_proto::{LastSeenVisibility, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};

/// What is stored about when a user was last active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredLastSeen {
  pub maybe_at: Option<OffsetDateTime>,
  pub visibility: LastSeenVisibility,
  /// The user chose "appear offline": their last seen is hidden whatever `visibility` says.
  pub is_invisible: bool,
}

impl Database {
  /// Records that the user stopped being active at `at`.
  pub async fn set_last_seen(&self, user: UserId, at: OffsetDateTime) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE users SET last_seen_at = $2 WHERE id = $1",
      to_db(user),
      at
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn get_last_seen_visibility(&self, user: UserId) -> Result<LastSeenVisibility, DbError> {
    let visibility = sqlx::query_scalar!(
      "SELECT last_seen_visibility FROM users WHERE id = $1",
      to_db(user)
    )
    .fetch_one(self.pool())
    .await?;
    Ok(LastSeenVisibility::from(visibility))
  }

  pub async fn set_last_seen_visibility(
    &self,
    user: UserId,
    visibility: &LastSeenVisibility,
  ) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE users SET last_seen_visibility = $2 WHERE id = $1",
      to_db(user),
      visibility.as_str(),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Last seen times and visibility choices of the given users.
  pub async fn get_last_seen(&self, users: &[UserId]) -> Result<HashMap<UserId, StoredLastSeen>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, last_seen_at, last_seen_visibility, presence_pref = 'invisible' AS "is_invisible!"
         FROM users WHERE id = ANY($1)"#,
      &to_db_all(users),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok((
          from_db(row.id)?,
          StoredLastSeen {
            maybe_at: row.last_seen_at,
            visibility: LastSeenVisibility::from(row.last_seen_visibility),
            is_invisible: row.is_invisible,
          },
        ))
      })
      .collect()
  }
}
