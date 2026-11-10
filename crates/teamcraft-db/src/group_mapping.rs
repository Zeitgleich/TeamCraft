//! Identity provider groups to spaces and public channels (docs/provisioning.md §5): the rules, and
//! which account got which revision of a rule.

use teamcraft_proto::{ConversationId, GroupMappingId, SpaceId, UserId};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_all, to_db, to_db_all};
use crate::invite::AppliedInvite;

/// A rule as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMapping {
  pub id: GroupMappingId,
  pub group_path: String,
  pub space_ids: Vec<SpaceId>,
  pub channel_ids: Vec<ConversationId>,
  pub revision: i64,
}

/// A rule to store: an existing path keeps its ID, and its revision rises if its targets changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGroupMapping {
  pub id: GroupMappingId,
  pub group_path: String,
  pub space_ids: Vec<SpaceId>,
  pub channel_ids: Vec<ConversationId>,
}

impl Database {
  /// Every rule, by path.
  pub async fn list_group_mappings(&self) -> Result<Vec<GroupMapping>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, group_path, space_ids, channel_ids, revision
       FROM identity_provider_group_mappings ORDER BY group_path"
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(GroupMapping {
          id: from_db(row.id)?,
          group_path: row.group_path,
          space_ids: from_db_all(&row.space_ids)?,
          channel_ids: from_db_all(&row.channel_ids)?,
          revision: row.revision,
        })
      })
      .collect()
  }

  /// Of `spaces`, those that exist and aren't deleted; of `channels`, the public channels of those
  /// spaces. What a rule names beyond that is refused.
  pub async fn group_mapping_targets(
    &self,
    spaces: &[SpaceId],
    channels: &[ConversationId],
  ) -> Result<(Vec<SpaceId>, Vec<ConversationId>), DbError> {
    let known_spaces = sqlx::query_scalar!(
      "SELECT id FROM spaces WHERE id = ANY($1) AND deleted_at IS NULL",
      &to_db_all(spaces),
    )
    .fetch_all(self.pool())
    .await?;
    let public_channels = sqlx::query_scalar!(
      "SELECT id FROM conversations
       WHERE id = ANY($1) AND space_id = ANY($2) AND kind = 'channel' AND visibility = 'public'
         AND deleted_at IS NULL",
      &to_db_all(channels),
      &known_spaces,
    )
    .fetch_all(self.pool())
    .await?;
    Ok((from_db_all(&known_spaces)?, from_db_all(&public_channels)?))
  }

  /// Whether any rule exists (a first sign-in then asks the sync for a run).
  pub async fn has_group_mappings(&self) -> Result<bool, DbError> {
    let has =
      sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM identity_provider_group_mappings) AS "has!""#)
        .fetch_one(self.pool())
        .await?;
    Ok(has)
  }

  /// Replaces all rules with `mappings` in one transaction. A path that stays keeps its ID; its
  /// revision rises only if its spaces or channels changed.
  pub async fn replace_group_mappings(&self, mappings: &[NewGroupMapping]) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    let paths: Vec<String> = mappings
      .iter()
      .map(|mapping| mapping.group_path.clone())
      .collect();
    sqlx::query!(
      "DELETE FROM identity_provider_group_mappings WHERE NOT (group_path = ANY($1))",
      &paths,
    )
    .execute(&mut *tx)
    .await?;
    for mapping in mappings {
      sqlx::query!(
        "INSERT INTO identity_provider_group_mappings (id, group_path, space_ids, channel_ids)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (group_path) DO UPDATE
           SET space_ids = EXCLUDED.space_ids, channel_ids = EXCLUDED.channel_ids,
               revision = identity_provider_group_mappings.revision
                 + CASE WHEN identity_provider_group_mappings.space_ids IS DISTINCT FROM EXCLUDED.space_ids
                          OR identity_provider_group_mappings.channel_ids IS DISTINCT FROM EXCLUDED.channel_ids
                        THEN 1 ELSE 0 END",
        to_db(mapping.id),
        mapping.group_path,
        &to_db_all(&mapping.space_ids),
        &to_db_all(&mapping.channel_ids),
      )
      .execute(&mut *tx)
      .await?;
    }
    tx.commit().await?;
    Ok(())
  }

  /// Applies `mapping` to `user` unless they already got this revision: the rule's spaces (as a
  /// member) and, of those, its public channels. Archived and deleted ones are skipped, deactivated
  /// accounts too. What was newly joined is returned, so it can be announced.
  pub async fn apply_group_mapping(
    &self,
    mapping: &GroupMapping,
    user: UserId,
  ) -> Result<Vec<AppliedInvite>, DbError> {
    let mut tx = self.pool().begin().await?;
    let claimed = sqlx::query!(
      "INSERT INTO identity_provider_group_mapping_applied (mapping_id, user_id, revision)
       SELECT $1, u.id, $3 FROM users u WHERE u.id = $2 AND u.deactivated_at IS NULL AND u.deleted_at IS NULL
       ON CONFLICT (mapping_id, user_id) DO UPDATE
         SET revision = EXCLUDED.revision, applied_at = now()
         WHERE identity_provider_group_mapping_applied.revision < EXCLUDED.revision",
      to_db(mapping.id),
      to_db(user),
      mapping.revision,
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed == 0 {
      tx.commit().await?;
      return Ok(Vec::new());
    }
    let mut applied = Vec::new();
    for space in &mapping.space_ids {
      let is_space_open = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM spaces WHERE id = $1 AND archived_at IS NULL AND deleted_at IS NULL) AS "open!""#,
        to_db(*space),
      )
      .fetch_one(&mut *tx)
      .await?;
      if !is_space_open {
        continue;
      }
      let joined_space = sqlx::query!(
        "INSERT INTO space_members (space_id, user_id, role) VALUES ($1, $2, 'member')
         ON CONFLICT (space_id, user_id) DO NOTHING",
        to_db(*space),
        to_db(user),
      )
      .execute(&mut *tx)
      .await?
      .rows_affected();
      let joined_channels = sqlx::query_scalar!(
        r#"INSERT INTO memberships (conversation_id, user_id, role)
           SELECT c.id, $2, 'member' FROM conversations c
           WHERE c.space_id = $1 AND c.kind = 'channel' AND c.visibility = 'public' AND c.deleted_at IS NULL
             AND c.archived_at IS NULL AND c.id = ANY($3)
           ON CONFLICT DO NOTHING RETURNING conversation_id"#,
        to_db(*space),
        to_db(user),
        &to_db_all(&mapping.channel_ids),
      )
      .fetch_all(&mut *tx)
      .await?;
      if joined_space > 0 || !joined_channels.is_empty() {
        applied.push(AppliedInvite {
          space_id: *space,
          joined_channels: from_db_all(&joined_channels)?,
        });
      }
    }
    tx.commit().await?;
    Ok(applied)
  }
}
