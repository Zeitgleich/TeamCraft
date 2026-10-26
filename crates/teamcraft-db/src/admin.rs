use std::collections::BTreeMap;

use teamcraft_proto::{
  ConversationId, ConversationVisibility, DeviceId, JoinPolicy, PushProvider, SpaceId, UserId, UserKind,
};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};

#[derive(Debug, Clone)]
pub struct AdminSpaceRow {
  pub id: SpaceId,
  pub name: String,
  pub join_policy: JoinPolicy,
  pub is_archived: bool,
  pub member_count: i64,
  pub guest_count: i64,
  pub channel_count: i64,
  pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default)]
pub struct AdminConversationFilter {
  pub maybe_space: Option<SpaceId>,
  pub maybe_visibility: Option<ConversationVisibility>,
  pub maybe_has_guests: Option<bool>,
  pub maybe_inactive_days: Option<u32>,
  pub maybe_min_members: Option<u32>,
  pub maybe_max_members: Option<u32>,
  pub maybe_after: Option<ConversationId>,
  pub maybe_member: Option<UserId>,
  /// `Some(true)`: only channels without an active channel admin; `Some(false)`: only the others.
  pub maybe_is_orphaned: Option<bool>,
  pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct AdminConversationRow {
  pub id: ConversationId,
  pub maybe_space: Option<SpaceId>,
  pub maybe_name: Option<String>,
  pub visibility: ConversationVisibility,
  pub is_archived: bool,
  pub member_count: i64,
  pub guest_count: i64,
  pub message_count: i64,
  /// Bytes of the channel's attachments and search index segments in storage (ciphertext).
  pub storage_bytes: i64,
  pub created_at: OffsetDateTime,
  /// Creation time of the newest message, derived from its Snowflake ID.
  pub maybe_last_message_millis: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserStatusFilter {
  Active,
  Deactivated,
  All,
}

#[derive(Debug, Clone)]
pub struct AdminUserRow {
  pub id: UserId,
  pub maybe_username: Option<String>,
  pub display_name: String,
  pub kind: UserKind,
  pub maybe_oidc_role: Option<String>,
  pub is_deactivated: bool,
  /// A deleted account's tombstone (issue #630).
  pub is_deleted: bool,
  pub maybe_deactivation_reason: Option<String>,
  pub device_count: i64,
  pub space_count: i64,
  pub conversation_count: i64,
  pub maybe_last_seen: Option<OffsetDateTime>,
  pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct AdminDeviceRow {
  pub id: DeviceId,
  pub name: String,
  pub is_revoked: bool,
  pub maybe_push_provider: Option<PushProvider>,
  pub maybe_last_seen: Option<OffsetDateTime>,
  pub created_at: OffsetDateTime,
}

/// What deactivating a user touched, so the server can tell the people affected.
#[derive(Debug, Clone, Default)]
pub struct Deactivation {
  /// The user was already deactivated; nothing changed in the database.
  pub was_already_deactivated: bool,
  /// Conversations the user is (still) a member of.
  pub conversations: Vec<ConversationId>,
  /// Everyone who shares a conversation with the user.
  pub co_members: Vec<UserId>,
}

/// Outcome of removing a deactivated user from all chats.
#[derive(Debug, Clone)]
pub enum ChatRemoval {
  Removed(ChatRemovalEffects),
  /// The user is active; only deactivated users are removed this way.
  NotDeactivated,
  UnknownUser,
}

/// What removing a deactivated user from all chats removed, so the server can tell the people
/// affected.
#[derive(Debug, Clone, Default)]
pub struct ChatRemovalEffects {
  pub devices: Vec<DeviceId>,
  /// Conversations the user left, with the members that remain.
  pub conversations: Vec<(ConversationId, Vec<UserId>)>,
  /// Spaces the user left, with the members that remain.
  pub spaces: Vec<(SpaceId, Vec<UserId>)>,
  /// Everyone who shared a conversation with the user (audience of device removals).
  pub co_members: Vec<UserId>,
}

impl Database {
  pub async fn admin_list_spaces(&self) -> Result<Vec<AdminSpaceRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT s.id, s.name, s.join_policy, s.archived_at IS NOT NULL AS "is_archived!", s.created_at,
                (SELECT count(*) FROM space_members m WHERE m.space_id = s.id) AS "member_count!",
                (SELECT count(*) FROM space_members m JOIN users u ON u.id = m.user_id
                  WHERE m.space_id = s.id AND u.kind = 'guest') AS "guest_count!",
                (SELECT count(*) FROM conversations c WHERE c.space_id = s.id) AS "channel_count!"
         FROM spaces s ORDER BY s.name, s.id"#,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(AdminSpaceRow {
          id: from_db(row.id)?,
          name: row.name,
          join_policy: JoinPolicy::from(row.join_policy),
          is_archived: row.is_archived,
          member_count: row.member_count,
          guest_count: row.guest_count,
          channel_count: row.channel_count,
          created_at: row.created_at,
        })
      })
      .collect()
  }

  /// Channels only (DMs and group DMs are private to their participants), ascending by ID.
  pub async fn admin_list_conversations(
    &self,
    filter: &AdminConversationFilter,
  ) -> Result<Vec<AdminConversationRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT t.id AS "id!", t.space_id, t.name, t.visibility AS "visibility!", t.is_archived AS "is_archived!",
                t.created_at AS "created_at!", t.member_count AS "member_count!", t.guest_count AS "guest_count!",
                t.message_count AS "message_count!", t.last_message_id, t.storage_bytes AS "storage_bytes!"
         FROM (
           SELECT c.id, c.space_id, c.name, c.visibility, c.archived_at IS NOT NULL AS is_archived, c.created_at,
                  (SELECT count(*) FROM memberships m WHERE m.conversation_id = c.id) AS member_count,
                  (SELECT count(*) FROM memberships m JOIN users u ON u.id = m.user_id
                    WHERE m.conversation_id = c.id AND u.kind = 'guest') AS guest_count,
                  (SELECT count(*) FROM messages x WHERE x.conversation_id = c.id) AS message_count,
                  (SELECT max(x.id) FROM messages x WHERE x.conversation_id = c.id) AS last_message_id,
                  -- Ciphertext sizes: what the channel's files and search index take up in storage.
                  ((SELECT COALESCE(sum(a.size_bytes), 0) FROM attachments a
                     WHERE a.conversation_id = c.id AND a.completed_at IS NOT NULL)
                   + (SELECT COALESCE(sum(s.size_bytes), 0) FROM index_segments s
                     WHERE s.conversation_id = c.id))::bigint AS storage_bytes
           FROM conversations c
           WHERE c.kind = 'channel'
             AND ($1::bigint IS NULL OR c.space_id = $1)
             AND ($2::text IS NULL OR c.visibility = $2)
             AND ($3::bigint IS NULL OR c.id > $3)
             AND ($10::bigint IS NULL OR EXISTS (
                   SELECT 1 FROM memberships m WHERE m.conversation_id = c.id AND m.user_id = $10))
         ) t
         WHERE ($4::boolean IS NULL OR (t.guest_count > 0) = $4)
           AND ($5::bigint IS NULL OR t.member_count >= $5)
           AND ($6::bigint IS NULL OR t.member_count <= $6)
           AND ($7::int IS NULL OR COALESCE(t.last_message_id, tc_snowflake_floor(t.created_at))
                                   < tc_snowflake_floor(now() - make_interval(days => $7)))
           AND ($8::boolean IS NULL OR (NOT EXISTS (
                 SELECT 1 FROM memberships m JOIN users u ON u.id = m.user_id
                  WHERE m.conversation_id = t.id AND m.role = 'admin' AND u.deactivated_at IS NULL)) = $8)
         ORDER BY t.id LIMIT $9"#,
      filter.maybe_space.map(to_db),
      filter.maybe_visibility.as_ref().map(ConversationVisibility::as_str),
      filter.maybe_after.map(to_db),
      filter.maybe_has_guests,
      filter.maybe_min_members.map(i64::from),
      filter.maybe_max_members.map(i64::from),
      filter.maybe_inactive_days.map(|days| i32::try_from(days).unwrap_or(i32::MAX)),
      filter.maybe_is_orphaned,
      filter.limit + 1,
      filter.maybe_member.map(to_db),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(AdminConversationRow {
          id: from_db(row.id)?,
          maybe_space: from_db_opt(row.space_id)?,
          maybe_name: row.name,
          visibility: ConversationVisibility::from(row.visibility),
          is_archived: row.is_archived,
          member_count: row.member_count,
          guest_count: row.guest_count,
          message_count: row.message_count,
          storage_bytes: row.storage_bytes,
          created_at: row.created_at,
          maybe_last_message_millis: row
            .last_message_id
            .map(|id| (id >> 22) + teamcraft_proto::SNOWFLAKE_EPOCH_MS),
        })
      })
      .collect()
  }

  pub async fn admin_list_users(
    &self,
    query: &str,
    status: UserStatusFilter,
    maybe_after: Option<UserId>,
    limit: i64,
  ) -> Result<Vec<AdminUserRow>, DbError> {
    let pattern = format!(
      "%{}%",
      query
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
    );
    let status_text = match status {
      UserStatusFilter::Active => "active",
      UserStatusFilter::Deactivated => "deactivated",
      UserStatusFilter::All => "all",
    };
    let rows = sqlx::query!(
      r#"SELECT u.id, u.username, u.display_name, u.kind, u.oidc_role, u.deactivated_at IS NOT NULL AS "is_deactivated!",
                u.deleted_at IS NOT NULL AS "is_deleted!", u.deactivation_reason, u.created_at,
                (SELECT count(*) FROM devices d WHERE d.user_id = u.id AND d.revoked_at IS NULL) AS "device_count!",
                (SELECT count(*) FROM space_members m WHERE m.user_id = u.id) AS "space_count!",
                (SELECT count(*) FROM memberships m WHERE m.user_id = u.id) AS "conversation_count!",
                -- Only the day for users who hide their last seen or appear offline: admins still
                -- spot unused devices, but not when someone was active.
                (SELECT CASE WHEN u.last_seen_visibility = 'everyone' AND u.presence_pref <> 'invisible'
                             THEN max(d.last_seen_at)
                             ELSE date_trunc('day', max(d.last_seen_at), 'UTC') END
                 FROM devices d WHERE d.user_id = u.id) AS last_seen
         FROM users u
         WHERE (u.display_name ILIKE $1 OR u.username ILIKE $1)
           AND ($2 = 'all' OR ($2 = 'active') = (u.deactivated_at IS NULL))
           AND ($3::bigint IS NULL OR u.id > $3)
         ORDER BY u.id LIMIT $4"#,
      pattern,
      status_text,
      maybe_after.map(to_db),
      limit + 1,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(AdminUserRow {
          id: from_db(row.id)?,
          maybe_username: row.username,
          display_name: row.display_name,
          kind: UserKind::from(row.kind),
          maybe_oidc_role: row.oidc_role,
          is_deactivated: row.is_deactivated,
          is_deleted: row.is_deleted,
          maybe_deactivation_reason: row.deactivation_reason,
          device_count: row.device_count,
          space_count: row.space_count,
          conversation_count: row.conversation_count,
          maybe_last_seen: row.last_seen,
          created_at: row.created_at,
        })
      })
      .collect()
  }

  /// All devices of a user including revoked ones. Last activity only to the day if the user hides
  /// their last seen or appears offline.
  pub async fn admin_list_devices(&self, user: UserId) -> Result<Vec<AdminDeviceRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT d.id, d.name, d.revoked_at IS NOT NULL AS "is_revoked!", d.push_provider, d.created_at,
                CASE WHEN u.last_seen_visibility = 'everyone' AND u.presence_pref <> 'invisible'
                     THEN d.last_seen_at
                     ELSE date_trunc('day', d.last_seen_at, 'UTC') END AS last_seen_at
         FROM devices d JOIN users u ON u.id = d.user_id
         WHERE d.user_id = $1 ORDER BY d.id"#,
      to_db(user),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(AdminDeviceRow {
          id: from_db(row.id)?,
          name: row.name,
          is_revoked: row.is_revoked,
          maybe_push_provider: row.push_provider.map(PushProvider::from),
          maybe_last_seen: row.last_seen_at,
          created_at: row.created_at,
        })
      })
      .collect()
  }

  /// Deactivates (suspends) the account: the user can't sign in and gets no pushes (the push
  /// queries skip deactivated users), but memberships, space memberships, devices and their push
  /// registrations stay, so reactivating restores everything. Returns who to tell. Idempotent.
  pub async fn deactivate_user(&self, user: UserId, reason: &str) -> Result<Deactivation, DbError> {
    let mut tx = self.pool().begin().await?;
    let marked = sqlx::query!(
      "UPDATE users SET deactivated_at = now(), deactivation_reason = $2 WHERE id = $1 AND deactivated_at IS NULL",
      to_db(user),
      reason,
    )
    .execute(&mut *tx)
    .await?;
    // Also when it already was: a caller that crashed after the commit repeats its steps.
    let was_already_deactivated = marked.rows_affected() == 0;
    let conversations = sqlx::query_scalar!(
      "SELECT conversation_id FROM memberships WHERE user_id = $1 ORDER BY conversation_id",
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;
    let co_members = sqlx::query_scalar!(
      r#"SELECT DISTINCT o.user_id FROM memberships m
         JOIN memberships o ON o.conversation_id = m.conversation_id AND o.user_id <> m.user_id
         WHERE m.user_id = $1 ORDER BY o.user_id"#,
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Deactivation {
      was_already_deactivated,
      conversations: conversations.into_iter().map(from_db).collect::<Result<_, _>>()?,
      co_members: co_members.into_iter().map(from_db).collect::<Result<_, _>>()?,
    })
  }

  /// Removes a deactivated user from everything: devices are revoked and the user leaves all
  /// conversations and spaces. Returns what changed so others can be told (their clients commit
  /// the removals to the MLS groups). Refuses active users: they are deactivated first.
  pub async fn remove_deactivated_user_from_chats(&self, user: UserId) -> Result<ChatRemoval, DbError> {
    let mut tx = self.pool().begin().await?;
    // Locks the row so a concurrent reactivation waits until the removal is done.
    let is_deactivated = sqlx::query_scalar!(
      r#"SELECT deactivated_at IS NOT NULL AS "is_deactivated!" FROM users WHERE id = $1 FOR UPDATE"#,
      to_db(user),
    )
    .fetch_optional(&mut *tx)
    .await?;
    match is_deactivated {
      None => return Ok(ChatRemoval::UnknownUser),
      Some(false) => return Ok(ChatRemoval::NotDeactivated),
      Some(true) => {},
    }

    let devices = sqlx::query_scalar!(
      "UPDATE devices SET revoked_at = now(), push_provider = NULL, push_token = NULL, push_key = NULL
       WHERE user_id = $1 AND revoked_at IS NULL RETURNING id",
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;

    let conversation_rows = sqlx::query!(
      r#"SELECT m.conversation_id AS "conversation_id!", o.user_id AS "other?" FROM memberships m
         LEFT JOIN memberships o ON o.conversation_id = m.conversation_id AND o.user_id <> m.user_id
         WHERE m.user_id = $1"#,
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;
    let space_rows = sqlx::query!(
      r#"SELECT m.space_id AS "space_id!", o.user_id AS "other?" FROM space_members m
         LEFT JOIN space_members o ON o.space_id = m.space_id AND o.user_id <> m.user_id
         WHERE m.user_id = $1"#,
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!("DELETE FROM memberships WHERE user_id = $1", to_db(user))
      .execute(&mut *tx)
      .await?;
    sqlx::query!("DELETE FROM space_members WHERE user_id = $1", to_db(user))
      .execute(&mut *tx)
      .await?;
    tx.commit().await?;

    let mut effects = ChatRemovalEffects {
      devices: devices.into_iter().map(from_db).collect::<Result<_, _>>()?,
      ..ChatRemovalEffects::default()
    };
    let mut conversations: BTreeMap<i64, Vec<UserId>> = BTreeMap::new();
    for row in conversation_rows {
      let entry = conversations.entry(row.conversation_id).or_default();
      if let Some(other) = row.other {
        entry.push(from_db(other)?);
      }
    }
    for (id, members) in conversations {
      effects.co_members.extend(members.iter().copied());
      effects.conversations.push((from_db(id)?, members));
    }
    let mut spaces: BTreeMap<i64, Vec<UserId>> = BTreeMap::new();
    for row in space_rows {
      let entry = spaces.entry(row.space_id).or_default();
      if let Some(other) = row.other {
        entry.push(from_db(other)?);
      }
    }
    for (id, members) in spaces {
      effects.spaces.push((from_db(id)?, members));
    }
    effects.co_members.sort_unstable();
    effects.co_members.dedup();
    Ok(ChatRemoval::Removed(effects))
  }

  /// Users deactivated before `cutoff` who are still in a conversation or space or still have a
  /// device: due for the automatic removal from all chats. At most `limit`, oldest first. Users
  /// the identity provider sync deactivated in a burst (at least `mass_oidc_count` within an hour
  /// of each other, e.g. a provider outage or misconfiguration) are left out: an admin decides.
  pub async fn deactivated_users_due_for_removal(
    &self,
    cutoff: OffsetDateTime,
    mass_oidc_count: i64,
    limit: i64,
  ) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      r#"SELECT u.id FROM users u
         WHERE u.deactivated_at < $1
           AND (EXISTS (SELECT 1 FROM memberships m WHERE m.user_id = u.id)
             OR EXISTS (SELECT 1 FROM space_members m WHERE m.user_id = u.id)
             OR EXISTS (SELECT 1 FROM devices d WHERE d.user_id = u.id AND d.revoked_at IS NULL))
           AND NOT (u.deactivation_reason = 'oidc' AND (
             SELECT count(*) FROM users o
             WHERE o.deactivation_reason = 'oidc'
               AND o.deactivated_at BETWEEN u.deactivated_at - interval '1 hour'
                                        AND u.deactivated_at + interval '1 hour') >= $3)
         ORDER BY u.deactivated_at, u.id LIMIT $2"#,
      cutoff,
      limit,
      mass_oidc_count,
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// Lifts the deactivation. Memberships and devices the user still has work again as before; after
  /// a removal from all chats they stay removed. A deleted account stays deleted (`false`).
  pub async fn reactivate_user(&self, user: UserId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE users SET deactivated_at = NULL, deactivation_reason = NULL
       WHERE id = $1 AND deactivated_at IS NOT NULL AND deleted_at IS NULL",
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }
}

/// Live devices per push provider (devices without a push channel are not counted).
#[derive(Debug, Clone)]
pub struct PushDeviceCount {
  pub provider: String,
  pub device_count: u64,
}

/// What attachments and search index segments take up in object storage.
#[derive(Debug, Clone, Copy, Default)]
pub struct StorageTotals {
  pub attachment_count: u64,
  pub attachment_bytes: u64,
  pub index_segment_bytes: u64,
}

impl Database {
  pub async fn admin_push_device_counts(&self) -> Result<Vec<PushDeviceCount>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT push_provider AS "provider!", count(*) AS "device_count!"
         FROM devices WHERE revoked_at IS NULL AND push_provider IS NOT NULL
         GROUP BY push_provider"#,
    )
    .fetch_all(self.pool())
    .await?;
    Ok(
      rows
        .into_iter()
        .map(|row| PushDeviceCount {
          provider: row.provider,
          device_count: u64::try_from(row.device_count).unwrap_or(0),
        })
        .collect(),
    )
  }

  pub async fn admin_storage_totals(&self) -> Result<StorageTotals, DbError> {
    let attachments = sqlx::query!(
      r#"SELECT count(*) AS "count!", COALESCE(sum(size_bytes), 0)::bigint AS "bytes!"
         FROM attachments WHERE completed_at IS NOT NULL"#,
    )
    .fetch_one(self.pool())
    .await?;
    let segments =
      sqlx::query!(r#"SELECT COALESCE(sum(size_bytes), 0)::bigint AS "bytes!" FROM index_segments"#,)
        .fetch_one(self.pool())
        .await?;
    Ok(StorageTotals {
      attachment_count: u64::try_from(attachments.count).unwrap_or(0),
      attachment_bytes: u64::try_from(attachments.bytes).unwrap_or(0),
      index_segment_bytes: u64::try_from(segments.bytes).unwrap_or(0),
    })
  }
}
