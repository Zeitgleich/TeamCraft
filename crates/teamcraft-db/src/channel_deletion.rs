//! Leaving and deleting channels and spaces (issue #1130, e2ee.md §4.6). A check, the change and who
//! remains happen under one lock on the conversation, so a leave, a join, a restore and a deletion at
//! the same moment see each other. Deleting only marks the row, removes the memberships and records the
//! deletion for the members' devices, in one transaction; background work then clears storage and rows in batches, because a big channel would outlast the request.

use sqlx::{Postgres, Transaction};
use teamcraft_proto::{ChannelRole, ConversationId, SpaceId, UserId};

use crate::audit::NewAuditEntry;
use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db, to_db_all};

/// Messages removed in one statement while a channel's rows are cleared.
const MESSAGE_BATCH: i64 = 5_000;

/// What a removal is decided on, read under the conversation's lock.
#[derive(Debug, Clone)]
pub struct ChannelFacts {
  pub is_channel: bool,
  pub is_private: bool,
  pub is_archived: bool,
  pub is_imported: bool,
  pub maybe_importer: Option<UserId>,
  pub members: Vec<(UserId, ChannelRole)>,
}

/// What a removal comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalStep {
  /// The user isn't a member any more.
  Nothing,
  Remove,
  /// The last member leaves a private channel and said to delete it.
  DeleteChannel,
}

/// What happened, with who was in the conversation before.
#[derive(Debug, Clone)]
pub struct RemovalDone {
  pub step: RemovalStep,
  pub was_importer: bool,
  pub members: Vec<UserId>,
}

/// A channel of a space being deleted.
#[derive(Debug, Clone)]
pub struct SpaceChannelFacts {
  pub is_private: bool,
  pub maybe_importer: Option<UserId>,
  pub members: Vec<UserId>,
}

/// A marked deletion: the conversations with who was in them, and who was in the space.
#[derive(Debug, Clone, Default)]
pub struct SpaceDeletion {
  pub channels: Vec<(ConversationId, Vec<UserId>)>,
  pub members: Vec<UserId>,
}

/// What leaving a space came to.
#[derive(Debug, Clone, Default)]
pub struct SpaceLeft {
  pub left: Vec<ConversationId>,
  /// Those of them the leaver was the last member of, deleted by it.
  pub deleted: Vec<ConversationId>,
}

/// What a confirmation says and whether the caller may go on: the channels of a space, and those
/// private ones the viewer isn't in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpaceChannelCounts {
  pub channels: u32,
  pub hidden_private: u32,
}

impl Database {
  /// The facts without changing anything: the check a client asks before it touches the MLS group.
  pub async fn channel_facts(&self, conversation: ConversationId) -> Result<Option<ChannelFacts>, DbError> {
    let mut tx = self.pool().begin().await?;
    let facts = lock_facts(&mut tx, conversation, false).await?;
    tx.commit().await?;
    Ok(facts)
  }

  /// Removes `user` as `decide` says, under the conversation's lock. A deletion also marks the
  /// channel and writes `audit` in the same transaction.
  pub async fn remove_member_locked<E: From<DbError>>(
    &self,
    conversation: ConversationId,
    user: UserId,
    audit: &NewAuditEntry<'_>,
    decide: impl FnOnce(&ChannelFacts) -> Result<RemovalStep, E>,
  ) -> Result<RemovalDone, E> {
    let mut tx = self.pool().begin().await.map_err(DbError::from)?;
    let Some(facts) = lock_facts(&mut tx, conversation, true).await? else {
      return Ok(RemovalDone {
        step: RemovalStep::Nothing,
        was_importer: false,
        members: Vec::new(),
      });
    };
    let step = decide(&facts)?;
    let members: Vec<UserId> = facts.members.iter().map(|(member, _)| *member).collect();
    let was_importer = step == RemovalStep::Remove && facts.maybe_importer == Some(user);
    match step {
      RemovalStep::Nothing => {},
      RemovalStep::Remove => {
        sqlx::query!(
          "UPDATE conversations SET importer_user_id = NULL WHERE id = $1 AND importer_user_id = $2",
          to_db(conversation),
          to_db(user),
        )
        .execute(&mut *tx)
        .await
        .map_err(DbError::from)?;
        sqlx::query!(
          "DELETE FROM memberships WHERE conversation_id = $1 AND user_id = $2",
          to_db(conversation),
          to_db(user),
        )
        .execute(&mut *tx)
        .await
        .map_err(DbError::from)?;
      },
      RemovalStep::DeleteChannel => {
        mark_deleted(&mut tx, conversation, &members).await?;
        Self::record_audit_in(&mut tx, audit).await?;
      },
    }
    tx.commit().await.map_err(DbError::from)?;
    Ok(RemovalDone {
      step,
      was_importer,
      members,
    })
  }

  /// Marks a channel deleted if `decide` allows it, with one audit row. `None` when it is gone.
  pub async fn delete_channel_locked<E: From<DbError>>(
    &self,
    conversation: ConversationId,
    audit: &NewAuditEntry<'_>,
    decide: impl FnOnce(&ChannelFacts) -> Result<(), E>,
  ) -> Result<Option<Vec<UserId>>, E> {
    let mut tx = self.pool().begin().await.map_err(DbError::from)?;
    let Some(facts) = lock_facts(&mut tx, conversation, true).await? else {
      return Ok(None);
    };
    decide(&facts)?;
    let members: Vec<UserId> = facts.members.iter().map(|(member, _)| *member).collect();
    mark_deleted(&mut tx, conversation, &members).await?;
    Self::record_audit_in(&mut tx, audit).await?;
    tx.commit().await.map_err(DbError::from)?;
    Ok(Some(members))
  }

  /// Marks a space and all of its conversations deleted if `decide` allows it, with one audit row.
  /// `None` when it is gone.
  pub async fn delete_space_locked<E: From<DbError>>(
    &self,
    space: SpaceId,
    audit: &NewAuditEntry<'_>,
    decide: impl FnOnce(&[SpaceChannelFacts]) -> Result<(), E>,
  ) -> Result<Option<SpaceDeletion>, E> {
    let mut tx = self.pool().begin().await.map_err(DbError::from)?;
    let is_there = sqlx::query_scalar!(
      "SELECT id FROM spaces WHERE id = $1 AND deleted_at IS NULL FOR UPDATE",
      to_db(space),
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(DbError::from)?;
    if is_there.is_none() {
      return Ok(None);
    }
    let rows = sqlx::query!(
      r#"SELECT id, visibility, importer_user_id FROM conversations
         WHERE space_id = $1 AND deleted_at IS NULL ORDER BY id FOR UPDATE"#,
      to_db(space),
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(DbError::from)?;
    let mut deletion = SpaceDeletion::default();
    let mut facts = Vec::with_capacity(rows.len());
    for row in rows {
      let members = member_ids(&mut tx, row.id).await?;
      facts.push(SpaceChannelFacts {
        is_private: row.visibility == "private",
        maybe_importer: from_db_opt(row.importer_user_id)?,
        members: members.clone(),
      });
      deletion.channels.push((from_db(row.id)?, members));
    }
    decide(&facts)?;
    for (channel, members) in &deletion.channels {
      mark_deleted(&mut tx, *channel, members).await?;
    }
    let space_members = sqlx::query_scalar!(
      "DELETE FROM space_members WHERE space_id = $1 RETURNING user_id",
      to_db(space),
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(DbError::from)?;
    deletion.members = space_members
      .into_iter()
      .map(from_db)
      .collect::<Result<_, DbError>>()?;
    sqlx::query!("UPDATE spaces SET deleted_at = now() WHERE id = $1", to_db(space))
      .execute(&mut *tx)
      .await
      .map_err(DbError::from)?;
    Self::record_audit_in(&mut tx, audit).await?;
    tx.commit().await.map_err(DbError::from)?;
    Ok(Some(deletion))
  }

  /// What deleting these spaces would take, as `viewer` sees it.
  pub async fn space_channel_counts(
    &self,
    viewer: UserId,
    spaces: &[SpaceId],
  ) -> Result<Vec<(SpaceId, SpaceChannelCounts)>, DbError> {
    let ids: Vec<i64> = spaces.iter().map(|space| to_db(*space)).collect();
    let rows = sqlx::query!(
      r#"SELECT c.space_id AS "space_id!", count(*) AS "channels!",
                count(*) FILTER (WHERE c.visibility = 'private' AND m.user_id IS NULL) AS "hidden!"
         FROM conversations c
         LEFT JOIN memberships m ON m.conversation_id = c.id AND m.user_id = $1
         WHERE c.space_id = ANY($2) AND c.deleted_at IS NULL
         GROUP BY c.space_id"#,
      to_db(viewer),
      &ids,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok((
          from_db(row.space_id)?,
          SpaceChannelCounts {
            channels: u32::try_from(row.channels).unwrap_or(u32::MAX),
            hidden_private: u32::try_from(row.hidden).unwrap_or(u32::MAX),
          },
        ))
      })
      .collect()
  }

  /// Channels marked deleted and not cleared yet, with their pictures' keys, at most `limit`.
  pub async fn marked_deleted_conversations(
    &self,
    limit: i64,
  ) -> Result<Vec<(ConversationId, Option<String>)>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, image_key FROM conversations WHERE deleted_at IS NOT NULL ORDER BY id LIMIT $1",
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.id)?, row.image_key)))
      .collect()
  }

  /// Clears the rows of a marked conversation, its objects being gone: messages in batches, then the
  /// row with what hangs on it. Finished attachments go; an upload still running stays without a
  /// conversation, and the sweep of unfinished uploads frees what it leaves. Idempotent.
  pub async fn clear_deleted_conversation(&self, conversation: ConversationId) -> Result<(), DbError> {
    loop {
      let removed = sqlx::query!(
        r#"DELETE FROM messages WHERE id IN
             (SELECT id FROM messages WHERE conversation_id = $1 ORDER BY id LIMIT $2)"#,
        to_db(conversation),
        MESSAGE_BATCH,
      )
      .execute(self.pool())
      .await?
      .rows_affected();
      if removed == 0 {
        break;
      }
    }
    loop {
      let removed = sqlx::query!(
        r#"DELETE FROM mls_messages WHERE id IN
             (SELECT id FROM mls_messages WHERE conversation_id = $1 ORDER BY id LIMIT $2)"#,
        to_db(conversation),
        MESSAGE_BATCH,
      )
      .execute(self.pool())
      .await?
      .rows_affected();
      if removed == 0 {
        break;
      }
    }
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      "DELETE FROM attachments WHERE conversation_id = $1 AND completed_at IS NOT NULL",
      to_db(conversation),
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
      "DELETE FROM conversations WHERE id = $1 AND deleted_at IS NOT NULL",
      to_db(conversation),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
  }

  /// Spaces marked deleted whose conversations are cleared, with their pictures' keys.
  pub async fn cleared_deleted_spaces(&self, limit: i64) -> Result<Vec<(SpaceId, Option<String>)>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT s.id, s.image_key FROM spaces s
         WHERE s.deleted_at IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM conversations c WHERE c.space_id = s.id)
         ORDER BY s.id LIMIT $1"#,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.id)?, row.image_key)))
      .collect()
  }

  pub async fn clear_deleted_space(&self, space: SpaceId) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM spaces WHERE id = $1 AND deleted_at IS NOT NULL",
      to_db(space),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }
}

/// Reads what a removal is decided on, with the row locked when `is_for_update`. `None` when the
/// conversation is gone or marked deleted.
async fn lock_facts(
  tx: &mut Transaction<'_, Postgres>,
  conversation: ConversationId,
  is_for_update: bool,
) -> Result<Option<ChannelFacts>, DbError> {
  let row = sqlx::query!(
    r#"SELECT kind, visibility, archived_at IS NOT NULL AS "is_archived!", importer_user_id,
              imported_through_message_id, deleted_at IS NOT NULL AS "is_deleted!"
       FROM conversations WHERE id = $1"#,
    to_db(conversation),
  )
  .fetch_optional(&mut **tx)
  .await?;
  if is_for_update {
    sqlx::query_scalar!(
      "SELECT id FROM conversations WHERE id = $1 FOR UPDATE",
      to_db(conversation),
    )
    .fetch_optional(&mut **tx)
    .await?;
  }
  let Some(row) = row.filter(|row| !row.is_deleted) else {
    return Ok(None);
  };
  let members = sqlx::query!(
    "SELECT user_id, role FROM memberships WHERE conversation_id = $1",
    to_db(conversation),
  )
  .fetch_all(&mut **tx)
  .await?;
  Ok(Some(ChannelFacts {
    is_channel: row.kind == "channel",
    is_private: row.visibility == "private",
    is_archived: row.is_archived,
    is_imported: row.imported_through_message_id.is_some(),
    maybe_importer: from_db_opt(row.importer_user_id)?,
    members: members
      .into_iter()
      .map(|member| Ok((from_db(member.user_id)?, ChannelRole::from(member.role))))
      .collect::<Result<_, DbError>>()?,
  }))
}

async fn member_ids(tx: &mut Transaction<'_, Postgres>, conversation: i64) -> Result<Vec<UserId>, DbError> {
  let ids = sqlx::query_scalar!(
    "SELECT user_id FROM memberships WHERE conversation_id = $1",
    conversation,
  )
  .fetch_all(&mut **tx)
  .await?;
  ids.into_iter().map(from_db).collect()
}

async fn mark_deleted(
  tx: &mut Transaction<'_, Postgres>,
  conversation: ConversationId,
  members: &[UserId],
) -> Result<(), DbError> {
  sqlx::query!(
    "UPDATE conversations SET deleted_at = now() WHERE id = $1",
    to_db(conversation),
  )
  .execute(&mut **tx)
  .await?;
  sqlx::query!(
    r#"INSERT INTO deleted_conversations (user_id, conversation_id)
       SELECT unnest($1::bigint[]), $2 ON CONFLICT DO NOTHING"#,
    &to_db_all(members),
    to_db(conversation),
  )
  .execute(&mut **tx)
  .await?;
  sqlx::query!(
    "DELETE FROM memberships WHERE conversation_id = $1",
    to_db(conversation),
  )
  .execute(&mut **tx)
  .await?;
  Ok(())
}

/// Leaving a space: of the channels `user` just left, marks those nobody is in any more deleted, if
/// private and not imported, and records it for `user`'s other devices. Returns them.
pub(crate) async fn mark_memberless_private_deleted(
  tx: &mut Transaction<'_, Postgres>,
  left: &[i64],
  user: UserId,
) -> Result<Vec<i64>, DbError> {
  let deleted = sqlx::query_scalar!(
    r#"UPDATE conversations c SET deleted_at = now()
       WHERE c.id = ANY($1) AND c.kind = 'channel' AND c.visibility = 'private'
         AND c.imported_through_message_id IS NULL AND c.importer_user_id IS NULL
         AND NOT EXISTS (SELECT 1 FROM memberships m WHERE m.conversation_id = c.id)
       RETURNING c.id"#,
    left,
  )
  .fetch_all(&mut **tx)
  .await?;
  sqlx::query!(
    r#"INSERT INTO deleted_conversations (user_id, conversation_id)
       SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING"#,
    to_db(user),
    &deleted,
  )
  .execute(&mut **tx)
  .await?;
  Ok(deleted)
}
