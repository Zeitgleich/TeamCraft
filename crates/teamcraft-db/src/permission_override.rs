//! Changes to the permission scheme, per scope.

use teamcraft_proto::SpaceId;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// A stored change: `None` scope is the instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredOverride {
  pub maybe_space: Option<SpaceId>,
  pub role: String,
  pub permission: String,
  pub is_grant: bool,
}

/// A change to apply: `None` removes the stored one.
#[derive(Debug, Clone)]
pub struct OverrideEdit {
  pub role: String,
  pub permission: String,
  pub maybe_is_grant: Option<bool>,
}

impl Database {
  pub async fn list_permission_overrides(&self) -> Result<Vec<StoredOverride>, DbError> {
    let rows = sqlx::query!(
      "SELECT scope_id, role, permission, effect FROM permission_overrides ORDER BY scope_id, role, permission"
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(StoredOverride {
          maybe_space: if row.scope_id == 0 {
            None
          } else {
            Some(from_db(row.scope_id)?)
          },
          role: row.role,
          permission: row.permission,
          is_grant: row.effect == "grant",
        })
      })
      .collect()
  }

  /// Applies the edits to one scope in one transaction.
  pub async fn edit_permission_overrides(
    &self,
    maybe_space: Option<SpaceId>,
    edits: &[OverrideEdit],
  ) -> Result<(), DbError> {
    let scope = maybe_space.map_or(0, to_db);
    let mut tx = self.pool().begin().await?;
    for edit in edits {
      match edit.maybe_is_grant {
        Some(is_grant) => {
          sqlx::query!(
            r#"INSERT INTO permission_overrides (scope_id, role, permission, effect) VALUES ($1, $2, $3, $4)
               ON CONFLICT (scope_id, role, permission) DO UPDATE SET effect = $4"#,
            scope,
            edit.role,
            edit.permission,
            if is_grant { "grant" } else { "revoke" },
          )
          .execute(&mut *tx)
          .await?;
        },
        None => {
          sqlx::query!(
            "DELETE FROM permission_overrides WHERE scope_id = $1 AND role = $2 AND permission = $3",
            scope,
            edit.role,
            edit.permission,
          )
          .execute(&mut *tx)
          .await?;
        },
      }
    }
    tx.commit().await?;
    Ok(())
  }
}
