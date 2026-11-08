use std::collections::BTreeMap;

use teamcraft_proto::{
  ConversationId, ConversationVisibility, DeviceId, JoinPolicy, PushProvider, Snowflake, SpaceId, UserId,
  UserKind,
};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db};
use crate::snowflake::SnowflakeGenerator;

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
  /// Part of the channel's or its space's name, any case.
  pub maybe_query: Option<String>,
  /// Only these channels.
  pub maybe_ids: Option<Vec<ConversationId>>,
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
  /// The first channel admin by display name: the channel's owner. `None`: nobody is admin.
  pub maybe_owner: Option<UserId>,
  /// The channel's own retention, then its space's (the inputs of the effective retention).
  pub own_retention: crate::retention::StoredRetention,
  pub space_retention: crate::retention::StoredRetention,
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
  /// Bots paused with it because this user created them (a first deactivation only).
  pub paused_bots: Vec<UserId>,
}

/// The audit entry a deactivation writes in its own transaction. `detail` is JSON text the
/// database adds the number of conversations to.
#[derive(Debug, Clone)]
pub struct DeactivationAudit<'a> {
  pub id: Snowflake,
  pub maybe_actor: Option<UserId>,
  pub detail: &'a str,
}

/// The deactivation reason of a bot its creator's deactivation paused.
pub const CREATOR_DEACTIVATED_REASON: &str = "creator_deactivated";

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
                (SELECT count(*) FROM conversations c WHERE c.space_id = s.id AND c.deleted_at IS NULL)
                  AS "channel_count!"
         FROM spaces s WHERE s.deleted_at IS NULL ORDER BY s.name, s.id"#,
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
    let maybe_pattern = filter
      .maybe_query
      .as_deref()
      .map(str::trim)
      .filter(|query| !query.is_empty())
      .map(like_pattern);
    let maybe_ids: Option<Vec<i64>> = filter
      .maybe_ids
      .as_ref()
      .map(|ids| ids.iter().copied().map(to_db).collect());
    let rows = sqlx::query!(
      r#"SELECT t.id AS "id!", t.space_id, t.name, t.visibility AS "visibility!", t.is_archived AS "is_archived!",
                t.created_at AS "created_at!", t.member_count AS "member_count!", t.guest_count AS "guest_count!",
                t.message_count AS "message_count!", t.last_message_id, t.storage_bytes AS "storage_bytes!",
                t.owner_id, t.own_age_days, t.own_messages, t.space_age_days, t.space_messages
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
                     WHERE s.conversation_id = c.id))::bigint AS storage_bytes,
                  -- Owner and retention come with the list: one request instead of two per channel.
                  (SELECT m.user_id FROM memberships m JOIN users u ON u.id = m.user_id
                    WHERE m.conversation_id = c.id AND m.role = 'admin'
                    ORDER BY u.display_name, u.id LIMIT 1) AS owner_id,
                  c.retention_max_age_days AS own_age_days, c.retention_max_messages AS own_messages,
                  (SELECT sp.retention_max_age_days FROM spaces sp WHERE sp.id = c.space_id) AS space_age_days,
                  (SELECT sp.retention_max_messages FROM spaces sp WHERE sp.id = c.space_id) AS space_messages
           FROM conversations c
           WHERE c.kind = 'channel' AND c.deleted_at IS NULL
             AND ($1::bigint IS NULL OR c.space_id = $1)
             AND ($2::text IS NULL OR c.visibility = $2)
             AND ($3::bigint IS NULL OR c.id > $3)
             AND ($10::bigint IS NULL OR EXISTS (
                   SELECT 1 FROM memberships m WHERE m.conversation_id = c.id AND m.user_id = $10))
             AND ($11::text IS NULL OR c.name ILIKE $11 OR EXISTS (
                   SELECT 1 FROM spaces sp WHERE sp.id = c.space_id AND sp.name ILIKE $11))
             AND ($12::bigint[] IS NULL OR c.id = ANY($12))
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
      maybe_pattern,
      maybe_ids.as_deref(),
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
          maybe_owner: from_db_opt(row.owner_id)?,
          own_retention: crate::retention::StoredRetention {
            maybe_max_age_days: row.own_age_days,
            maybe_max_messages: row.own_messages,
          },
          space_retention: crate::retention::StoredRetention {
            maybe_max_age_days: row.space_age_days,
            maybe_max_messages: row.space_messages,
          },
        })
      })
      .collect()
  }

  pub async fn admin_list_users(
    &self,
    query: &str,
    status: UserStatusFilter,
    maybe_role: Option<&str>,
    maybe_ids: Option<&[UserId]>,
    maybe_after: Option<UserId>,
    limit: i64,
  ) -> Result<Vec<AdminUserRow>, DbError> {
    let pattern = like_pattern(query);
    let maybe_ids: Option<Vec<i64>> = maybe_ids.map(|ids| ids.iter().copied().map(to_db).collect());
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
           AND ($5::text IS NULL OR u.oidc_role = $5)
           AND ($6::bigint[] IS NULL OR u.id = ANY($6))
         ORDER BY u.id LIMIT $4"#,
      pattern,
      status_text,
      maybe_after.map(to_db),
      limit + 1,
      maybe_role,
      maybe_ids.as_deref(),
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
  /// registrations stay, so reactivating restores everything. Everything that decides access
  /// happens in this transaction: the audit entry, the follow-up record and pausing the bots the
  /// user created go with it or not at all. Returns who to tell. Idempotent: a repeat changes
  /// nothing and reports what is there to tell.
  pub async fn deactivate_user(
    &self,
    user: UserId,
    reason: &str,
    audit: &DeactivationAudit<'_>,
    ids: &SnowflakeGenerator,
  ) -> Result<Deactivation, DbError> {
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
    let (conversations, co_members) = deactivation_audience(&mut tx, user).await?;
    let mut paused_bots = Vec::new();
    if !was_already_deactivated {
      insert_followup(&mut tx, user).await?;
      insert_deactivation_audit(&mut tx, audit, user, conversations.len()).await?;
      let bots = sqlx::query_scalar!(
        r#"UPDATE users SET deactivated_at = now(), deactivation_reason = 'creator_deactivated'
           WHERE deactivated_at IS NULL AND id IN (SELECT user_id FROM bots WHERE created_by = $1 AND revoked_at IS NULL)
           RETURNING id"#,
        to_db(user),
      )
      .fetch_all(&mut *tx)
      .await?;
      for bot in bots {
        let bot: UserId = from_db(bot)?;
        insert_followup(&mut tx, bot).await?;
        let bot_conversations: i64 = sqlx::query_scalar!(
          r#"SELECT count(*) AS "count!" FROM memberships WHERE user_id = $1"#,
          to_db(bot),
        )
        .fetch_one(&mut *tx)
        .await?;
        insert_deactivation_audit(
          &mut tx,
          &DeactivationAudit {
            id: ids.next_snowflake()?,
            maybe_actor: None,
            detail: r#"{"reason":"creator_deactivated","source":"system","note":"its creator was deactivated"}"#,
          },
          bot,
          usize::try_from(bot_conversations).unwrap_or(usize::MAX),
        )
        .await?;
        paused_bots.push(bot);
      }
    }
    tx.commit().await?;
    Ok(Deactivation {
      was_already_deactivated,
      conversations,
      co_members,
      paused_bots,
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
  /// a removal from all chats they stay removed. Bots paused because of it come back with it. A
  /// deleted account stays deleted (`None`); otherwise the bots that came back.
  pub async fn reactivate_user(&self, user: UserId) -> Result<Option<Vec<UserId>>, DbError> {
    let mut tx = self.pool().begin().await?;
    let result = sqlx::query!(
      "UPDATE users SET deactivated_at = NULL, deactivation_reason = NULL
       WHERE id = $1 AND deactivated_at IS NOT NULL AND deleted_at IS NULL",
      to_db(user),
    )
    .execute(&mut *tx)
    .await?;
    if result.rows_affected() == 0 {
      return Ok(None);
    }
    sqlx::query!(
      "DELETE FROM user_deactivation_followups WHERE user_id = $1",
      to_db(user),
    )
    .execute(&mut *tx)
    .await?;
    let bots = sqlx::query_scalar!(
      r#"UPDATE users SET deactivated_at = NULL, deactivation_reason = NULL
         WHERE deactivation_reason = 'creator_deactivated' AND deleted_at IS NULL
           AND id IN (SELECT user_id FROM bots WHERE created_by = $1 AND revoked_at IS NULL)
         RETURNING id"#,
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!(
      "DELETE FROM user_deactivation_followups WHERE user_id = ANY($1)",
      &bots,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(bots.into_iter().map(from_db).collect::<Result<_, _>>()?))
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

/// `%text%` for `ILIKE`, with the wildcards of `text` taken literally.
pub(crate) fn like_pattern(text: &str) -> String {
  format!(
    "%{}%",
    text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
  )
}

pub(crate) async fn deactivation_audience(
  connection: &mut sqlx::PgConnection,
  user: UserId,
) -> Result<(Vec<ConversationId>, Vec<UserId>), DbError> {
  let conversations = sqlx::query_scalar!(
    "SELECT conversation_id FROM memberships WHERE user_id = $1 ORDER BY conversation_id",
    to_db(user),
  )
  .fetch_all(&mut *connection)
  .await?;
  let co_members = sqlx::query_scalar!(
    r#"SELECT DISTINCT o.user_id FROM memberships m
       JOIN memberships o ON o.conversation_id = m.conversation_id AND o.user_id <> m.user_id
       WHERE m.user_id = $1 ORDER BY o.user_id"#,
    to_db(user),
  )
  .fetch_all(&mut *connection)
  .await?;
  Ok((
    conversations.into_iter().map(from_db).collect::<Result<_, _>>()?,
    co_members.into_iter().map(from_db).collect::<Result<_, _>>()?,
  ))
}

async fn insert_followup(connection: &mut sqlx::PgConnection, user: UserId) -> Result<(), DbError> {
  sqlx::query!(
    "INSERT INTO user_deactivation_followups (user_id) VALUES ($1) ON CONFLICT (user_id) DO NOTHING",
    to_db(user),
  )
  .execute(connection)
  .await?;
  Ok(())
}

async fn insert_deactivation_audit(
  connection: &mut sqlx::PgConnection,
  audit: &DeactivationAudit<'_>,
  user: UserId,
  conversations: usize,
) -> Result<(), DbError> {
  sqlx::query!(
    r#"INSERT INTO audit_log (id, actor_user_id, action, target_kind, target_id, detail)
       VALUES ($1, $2, 'user_deactivated', 'user', $3,
               ($4::text::jsonb || jsonb_build_object('conversations', $5::bigint)))"#,
    audit.id.get(),
    audit.maybe_actor.map(to_db),
    to_db(user),
    audit.detail,
    i64::try_from(conversations).unwrap_or(i64::MAX),
  )
  .execute(connection)
  .await?;
  Ok(())
}

#[cfg(test)]
mod like_tests {
  use super::like_pattern;

  #[test]
  fn wildcards_in_the_search_text_are_literal() {
    assert_eq!(like_pattern("a_b%c\\d"), "%a\\_b\\%c\\\\d%");
    assert_eq!(like_pattern(""), "%%");
  }
}
