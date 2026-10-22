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
  pub maybe_deactivation_reason: Option<String>,
  pub device_count: i64,
  pub space_count: i64,
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

/// What deactivating a user removed, so the server can tell the people affected.
#[derive(Debug, Clone, Default)]
pub struct DeactivationEffects {
  /// The user was already deactivated; nothing changed.
  pub was_already_deactivated: bool,
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
                t.message_count AS "message_count!", t.last_message_id
         FROM (
           SELECT c.id, c.space_id, c.name, c.visibility, c.archived_at IS NOT NULL AS is_archived, c.created_at,
                  (SELECT count(*) FROM memberships m WHERE m.conversation_id = c.id) AS member_count,
                  (SELECT count(*) FROM memberships m JOIN users u ON u.id = m.user_id
                    WHERE m.conversation_id = c.id AND u.kind = 'guest') AS guest_count,
                  (SELECT count(*) FROM messages x WHERE x.conversation_id = c.id) AS message_count,
                  (SELECT max(x.id) FROM messages x WHERE x.conversation_id = c.id) AS last_message_id
           FROM conversations c
           WHERE c.kind = 'channel'
             AND ($1::bigint IS NULL OR c.space_id = $1)
             AND ($2::text IS NULL OR c.visibility = $2)
             AND ($3::bigint IS NULL OR c.id > $3)
         ) t
         WHERE ($4::boolean IS NULL OR (t.guest_count > 0) = $4)
           AND ($5::bigint IS NULL OR t.member_count >= $5)
           AND ($6::bigint IS NULL OR t.member_count <= $6)
           AND ($7::int IS NULL OR COALESCE(t.last_message_id, tc_snowflake_floor(t.created_at))
                                   < tc_snowflake_floor(now() - make_interval(days => $7)))
         ORDER BY t.id LIMIT $8"#,
      filter.maybe_space.map(to_db),
      filter.maybe_visibility.as_ref().map(ConversationVisibility::as_str),
      filter.maybe_after.map(to_db),
      filter.maybe_has_guests,
      filter.maybe_min_members.map(i64::from),
      filter.maybe_max_members.map(i64::from),
      filter.maybe_inactive_days.map(|days| i32::try_from(days).unwrap_or(i32::MAX)),
      filter.limit + 1,
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
                u.deactivation_reason, u.created_at,
                (SELECT count(*) FROM devices d WHERE d.user_id = u.id AND d.revoked_at IS NULL) AS "device_count!",
                (SELECT count(*) FROM space_members m WHERE m.user_id = u.id) AS "space_count!",
                (SELECT max(d.last_seen_at) FROM devices d WHERE d.user_id = u.id) AS last_seen
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
          maybe_deactivation_reason: row.deactivation_reason,
          device_count: row.device_count,
          space_count: row.space_count,
          maybe_last_seen: row.last_seen,
          created_at: row.created_at,
        })
      })
      .collect()
  }

  /// All devices of a user including revoked ones.
  pub async fn admin_list_devices(&self, user: UserId) -> Result<Vec<AdminDeviceRow>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, name, revoked_at IS NOT NULL AS "is_revoked!", push_provider, last_seen_at, created_at
         FROM devices WHERE user_id = $1 ORDER BY id"#,
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

  /// Deactivates the account: devices are revoked, push registrations dropped, and the user leaves
  /// all conversations and spaces. Returns what changed so others can be told. Idempotent.
  pub async fn deactivate_user(&self, user: UserId, reason: &str) -> Result<DeactivationEffects, DbError> {
    let mut tx = self.pool().begin().await?;
    let marked = sqlx::query!(
      "UPDATE users SET deactivated_at = now(), deactivation_reason = $2 WHERE id = $1 AND deactivated_at IS NULL",
      to_db(user),
      reason,
    )
    .execute(&mut *tx)
    .await?;
    if marked.rows_affected() == 0 {
      return Ok(DeactivationEffects {
        was_already_deactivated: true,
        ..DeactivationEffects::default()
      });
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

    let mut effects = DeactivationEffects {
      devices: devices.into_iter().map(from_db).collect::<Result<_, _>>()?,
      ..DeactivationEffects::default()
    };
    let mut conversations: std::collections::BTreeMap<i64, Vec<UserId>> = std::collections::BTreeMap::new();
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
    let mut spaces: std::collections::BTreeMap<i64, Vec<UserId>> = std::collections::BTreeMap::new();
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
    Ok(effects)
  }

  /// Lifts the deactivation. Memberships and devices are not restored.
  pub async fn reactivate_user(&self, user: UserId) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE users SET deactivated_at = NULL, deactivation_reason = NULL WHERE id = $1 AND deactivated_at IS NOT NULL",
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }
}
