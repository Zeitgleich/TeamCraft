//! Inactive channels, spaces archived with their channels, and idle guest accounts (issue #212).
//! Metadata only: how long a channel has been quiet comes from the time in its newest message's ID.

use teamcraft_proto::{ConversationId, SpaceId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// A channel the inactivity policy may have to act on.
#[derive(Debug, Clone)]
pub struct InactivityCandidate {
  pub id: ConversationId,
  pub created_at: OffsetDateTime,
  /// Creation time of the newest message (from its ID), if the channel has one.
  pub maybe_last_message_millis: Option<i64>,
  /// When the owners were told about the current quiet spell.
  pub maybe_notified_at: Option<OffsetDateTime>,
}

/// A channel the policy archived, on its way to deletion.
#[derive(Debug, Clone)]
pub struct PolicyArchivedChannel {
  pub id: ConversationId,
  pub archived_at: OffsetDateTime,
  /// When the owners were told that it will be deleted.
  pub maybe_notified_at: Option<OffsetDateTime>,
}

/// What archiving a space did to its channels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceArchive {
  /// Channels archived along with the space; the ones that were archived already are not in it.
  pub channels: Vec<ConversationId>,
}

/// What restoring a space did to its channels.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpaceRestore {
  /// Channels the space archived that are open again.
  pub restored: Vec<ConversationId>,
  /// Channels the space archived that stay archived because nobody can run them (no channel admin
  /// among their members); a workspace admin assigns an owner first (admin.md §3.1.1).
  pub left_archived: Vec<ConversationId>,
}

impl Database {
  /// Open channels of open spaces that may need a notice or archiving at `now`: not snoozed, not
  /// being imported, and either quiet since before `quiet_before` or already noticed. Quietest first,
  /// at most `limit`.
  pub async fn list_inactivity_candidates(
    &self,
    now: OffsetDateTime,
    quiet_before: OffsetDateTime,
    limit: i64,
  ) -> Result<Vec<InactivityCandidate>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT c.id, c.created_at, c.inactivity_notified_at, lm.last_id
         FROM conversations c
         JOIN spaces s ON s.id = c.space_id AND s.archived_at IS NULL AND s.deleted_at IS NULL
         LEFT JOIN LATERAL (SELECT max(m.id) AS last_id FROM messages m WHERE m.conversation_id = c.id) lm ON true
         WHERE c.kind = 'channel' AND c.archived_at IS NULL AND c.deleted_at IS NULL
           AND c.importer_user_id IS NULL
           AND (c.inactivity_snoozed_until IS NULL OR c.inactivity_snoozed_until <= $1)
           AND (c.inactivity_notified_at IS NOT NULL
                OR COALESCE(lm.last_id, tc_snowflake_floor(c.created_at)) < tc_snowflake_floor($2))
         ORDER BY COALESCE(lm.last_id, tc_snowflake_floor(c.created_at)), c.id
         LIMIT $3"#,
      now,
      quiet_before,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(InactivityCandidate {
          id: from_db(row.id)?,
          created_at: row.created_at,
          maybe_last_message_millis: row
            .last_id
            .map(|id| (id >> 22) + teamcraft_proto::SNOWFLAKE_EPOCH_MS),
          maybe_notified_at: row.inactivity_notified_at,
        })
      })
      .collect()
  }

  /// Records that the owners were told, if the channel is still open and its notice is still
  /// `maybe_expected` (nobody snoozed or noticed it meanwhile). `false`: nothing was recorded.
  pub async fn record_inactivity_notice(
    &self,
    conversation: ConversationId,
    at: OffsetDateTime,
    maybe_expected: Option<OffsetDateTime>,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE conversations SET inactivity_notified_at = $2
         WHERE id = $1 AND deleted_at IS NULL
           AND inactivity_notified_at IS NOT DISTINCT FROM $3"#,
      to_db(conversation),
      at,
      maybe_expected,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Forgets the notice: the channel was written in after it.
  pub async fn clear_inactivity_notice(&self, conversation: ConversationId) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE conversations SET inactivity_notified_at = NULL WHERE id = $1",
      to_db(conversation),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Defers everything the policy would do to the channel until `until`, and forgets the notice, so a
  /// channel still quiet after the snooze is noticed afresh. `false` when the channel is gone.
  pub async fn snooze_inactivity(
    &self,
    conversation: ConversationId,
    until: OffsetDateTime,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE conversations SET inactivity_snoozed_until = $2, inactivity_notified_at = NULL
         WHERE id = $1 AND deleted_at IS NULL"#,
      to_db(conversation),
      until,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Archives a noticed channel as the policy: only if it is still open, still noticed at
  /// `notified_at`, not snoozed at `now`, and no message came after the notice (checked in the same
  /// statement, so a message racing the archiving wins). `false`: nothing was archived.
  pub async fn archive_inactive_channel(
    &self,
    conversation: ConversationId,
    notified_at: OffsetDateTime,
    now: OffsetDateTime,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"UPDATE conversations c SET archived_at = $3, archive_origin = 'inactivity', inactivity_notified_at = NULL
         WHERE c.id = $1 AND c.kind = 'channel' AND c.archived_at IS NULL AND c.deleted_at IS NULL
           AND c.importer_user_id IS NULL
           AND c.inactivity_notified_at = $2
           AND (c.inactivity_snoozed_until IS NULL OR c.inactivity_snoozed_until <= $3)
           AND NOT EXISTS (SELECT 1 FROM messages m
                            WHERE m.conversation_id = c.id AND m.id >= tc_snowflake_floor($2))"#,
      to_db(conversation),
      notified_at,
      now,
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() == 1)
  }

  /// Channels the policy archived at or before `archived_before`, not snoozed at `now`, oldest
  /// archive first.
  pub async fn list_policy_archived_channels(
    &self,
    now: OffsetDateTime,
    archived_before: OffsetDateTime,
    limit: i64,
  ) -> Result<Vec<PolicyArchivedChannel>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, archived_at AS "archived_at!", inactivity_notified_at
         FROM conversations
         WHERE archive_origin = 'inactivity' AND archived_at IS NOT NULL AND deleted_at IS NULL
           AND archived_at <= $2
           AND (inactivity_snoozed_until IS NULL OR inactivity_snoozed_until <= $1)
         ORDER BY archived_at, id LIMIT $3"#,
      now,
      archived_before,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(PolicyArchivedChannel {
          id: from_db(row.id)?,
          archived_at: row.archived_at,
          maybe_notified_at: row.inactivity_notified_at,
        })
      })
      .collect()
  }

  /// Who is told about a channel: its channel admins who are active, else the admins of its space.
  pub async fn inactivity_notice_recipients(
    &self,
    conversation: ConversationId,
  ) -> Result<Vec<UserId>, DbError> {
    let owners = sqlx::query_scalar!(
      r#"SELECT m.user_id FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.conversation_id = $1 AND m.role = 'admin' AND u.deactivated_at IS NULL
         ORDER BY m.user_id"#,
      to_db(conversation),
    )
    .fetch_all(self.pool())
    .await?;
    if !owners.is_empty() {
      return owners.into_iter().map(from_db).collect();
    }
    let admins = sqlx::query_scalar!(
      r#"SELECT sm.user_id FROM conversations c
         JOIN space_members sm ON sm.space_id = c.space_id AND sm.role = 'space_admin'
         JOIN users u ON u.id = sm.user_id
         WHERE c.id = $1 AND u.deactivated_at IS NULL
         ORDER BY sm.user_id"#,
      to_db(conversation),
    )
    .fetch_all(self.pool())
    .await?;
    admins.into_iter().map(from_db).collect()
  }

  /// Archives an open space and, in the same transaction, every channel in it that is open, marking
  /// those as archived with the space, so that [`Self::restore_space_with_channels`] reverts exactly
  /// that. `None`: the space is archived already, or gone.
  pub async fn archive_space_with_channels(
    &self,
    space: SpaceId,
    at: OffsetDateTime,
  ) -> Result<Option<SpaceArchive>, DbError> {
    let mut tx = self.pool().begin().await?;
    let found = sqlx::query_scalar!(
      "UPDATE spaces SET archived_at = $2 WHERE id = $1 AND archived_at IS NULL AND deleted_at IS NULL RETURNING id",
      to_db(space),
      at,
    )
    .fetch_optional(&mut *tx)
    .await?;
    if found.is_none() {
      return Ok(None);
    }
    let channels = sqlx::query_scalar!(
      r#"UPDATE conversations SET archived_at = $2, archive_origin = 'space', inactivity_notified_at = NULL
         WHERE space_id = $1 AND archived_at IS NULL AND deleted_at IS NULL RETURNING id"#,
      to_db(space),
      at,
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(SpaceArchive {
      channels: channels.into_iter().map(from_db).collect::<Result<_, _>>()?,
    }))
  }

  /// Restores an archived space and the channels it archived: those a channel admin (or nobody) can
  /// run. Channels archived by hand, before or after, or by the inactivity policy stay as they are.
  /// With `maybe_snooze_until`, the restored channels are snoozed, so a quiet one isn't archived
  /// again at once. `None`: the space is open already, or gone.
  pub async fn restore_space_with_channels(
    &self,
    space: SpaceId,
    maybe_snooze_until: Option<OffsetDateTime>,
  ) -> Result<Option<SpaceRestore>, DbError> {
    let mut tx = self.pool().begin().await?;
    let found = sqlx::query_scalar!(
      "UPDATE spaces SET archived_at = NULL WHERE id = $1 AND archived_at IS NOT NULL AND deleted_at IS NULL RETURNING id",
      to_db(space),
    )
    .fetch_optional(&mut *tx)
    .await?;
    if found.is_none() {
      return Ok(None);
    }
    let restored = sqlx::query_scalar!(
      r#"UPDATE conversations c SET archived_at = NULL, archive_origin = NULL, inactivity_notified_at = NULL,
                inactivity_snoozed_until = COALESCE($2, c.inactivity_snoozed_until)
         WHERE c.space_id = $1 AND c.archive_origin = 'space' AND c.deleted_at IS NULL
           AND (NOT EXISTS (SELECT 1 FROM memberships m WHERE m.conversation_id = c.id)
                OR EXISTS (SELECT 1 FROM memberships m WHERE m.conversation_id = c.id AND m.role = 'admin'))
         RETURNING c.id"#,
      to_db(space),
      maybe_snooze_until,
    )
    .fetch_all(&mut *tx)
    .await?;
    // What is left was archived with the space but can't be run: from here on it is archived by hand.
    let left_archived = sqlx::query_scalar!(
      r#"UPDATE conversations SET archive_origin = NULL
         WHERE space_id = $1 AND archive_origin = 'space' AND deleted_at IS NULL RETURNING id"#,
      to_db(space),
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(SpaceRestore {
      restored: restored.into_iter().map(from_db).collect::<Result<_, _>>()?,
      left_archived: left_archived.into_iter().map(from_db).collect::<Result<_, _>>()?,
    }))
  }

  /// Starts the clock of local guest accounts that belong to no space any more and stops it for those
  /// that do again. Only accounts that were used (a password chosen, a device registered): a guest who
  /// has not signed in yet has no space until the invite is applied at the first sign-in. Returns how
  /// many started.
  pub async fn track_memberless_guest_accounts(&self, now: OffsetDateTime) -> Result<u64, DbError> {
    sqlx::query!(
      r#"UPDATE users SET memberless_since = NULL
         WHERE memberless_since IS NOT NULL
           AND EXISTS (SELECT 1 FROM space_members m WHERE m.user_id = users.id)"#,
    )
    .execute(self.pool())
    .await?;
    let started = sqlx::query!(
      r#"UPDATE users u SET memberless_since = $1
         WHERE u.kind = 'guest' AND u.memberless_since IS NULL
           AND u.deactivated_at IS NULL AND u.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM local_accounts a WHERE a.user_id = u.id AND a.password_hash IS NOT NULL)
           AND EXISTS (SELECT 1 FROM devices d WHERE d.user_id = u.id)
           AND NOT EXISTS (SELECT 1 FROM space_members m WHERE m.user_id = u.id)"#,
      now,
    )
    .execute(self.pool())
    .await?;
    Ok(started.rows_affected())
  }

  /// Local guest accounts that have belonged to no space since before `memberless_before` and are
  /// still active, longest first.
  pub async fn list_idle_guest_accounts(
    &self,
    memberless_before: OffsetDateTime,
    limit: i64,
  ) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      r#"SELECT u.id FROM users u
         WHERE u.kind = 'guest' AND u.memberless_since <= $1
           AND u.deactivated_at IS NULL AND u.deleted_at IS NULL
           AND EXISTS (SELECT 1 FROM local_accounts a WHERE a.user_id = u.id AND a.password_hash IS NOT NULL)
           AND EXISTS (SELECT 1 FROM devices d WHERE d.user_id = u.id)
           AND NOT EXISTS (SELECT 1 FROM space_members m WHERE m.user_id = u.id)
         ORDER BY u.memberless_since, u.id LIMIT $2"#,
      memberless_before,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }
}
