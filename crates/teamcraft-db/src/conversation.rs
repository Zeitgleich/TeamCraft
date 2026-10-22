use std::collections::{HashMap, HashSet};

use teamcraft_proto::{
  CategoryId, ChannelModeration, ChannelRole, ConversationId, ConversationKind, ConversationVisibility,
  HistoryPolicy, MessageId, NotificationLevel, Permission, SpaceId, UserId,
};

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_opt, to_db, to_db_all};
use crate::user::User;

#[derive(Debug, Clone)]
pub struct Conversation {
  pub id: ConversationId,
  pub maybe_space_id: Option<SpaceId>,
  pub kind: ConversationKind,
  pub maybe_name: Option<String>,
  pub maybe_topic: Option<String>,
  pub maybe_description: Option<String>,
  pub visibility: ConversationVisibility,
  pub history_policy: HistoryPolicy,
  pub maybe_category_id: Option<CategoryId>,
  pub is_archived: bool,
  pub moderation: ChannelModeration,
}

#[derive(Debug, Clone)]
pub struct Membership {
  pub role: ChannelRole,
  pub notification_level: NotificationLevel,
  pub is_favorite: bool,
  pub maybe_last_read_message_id: Option<MessageId>,
  /// Smallest message ID created after the member joined; history cut-off for `from_join`.
  pub joined_floor: MessageId,
}

/// A conversation and the viewer's membership (`None`: not a member).
#[derive(Debug, Clone)]
pub struct ConversationView {
  pub conversation: Conversation,
  pub maybe_membership: Option<Membership>,
}

/// Which conversations to list for a viewer.
#[derive(Debug, Clone, Default)]
pub struct ConversationFilter {
  pub maybe_conversation: Option<ConversationId>,
  pub maybe_space: Option<SpaceId>,
  /// Only conversations the viewer is a member of.
  pub only_joined: bool,
  /// Only public, unarchived channels the viewer is not in yet (channel browser).
  pub only_joinable: bool,
}

#[derive(Debug, Clone)]
pub struct NewConversation<'a> {
  pub id: ConversationId,
  pub maybe_space: Option<SpaceId>,
  pub kind: ConversationKind,
  pub maybe_name: Option<&'a str>,
  pub maybe_topic: Option<&'a str>,
  pub maybe_description: Option<&'a str>,
  pub visibility: ConversationVisibility,
  pub history_policy: HistoryPolicy,
  pub maybe_category: Option<CategoryId>,
  /// Set for DMs and group DMs, see [`participants_key`].
  pub maybe_participants_key: Option<String>,
}

/// Fields to change; `None` keeps the stored value. Empty strings clear topic and description.
#[derive(Debug, Clone, Default)]
pub struct ConversationChanges<'a> {
  pub maybe_name: Option<&'a str>,
  pub maybe_topic: Option<&'a str>,
  pub maybe_description: Option<&'a str>,
  pub maybe_visibility: Option<ConversationVisibility>,
  pub maybe_category: Option<CategoryId>,
  pub clear_category: bool,
  pub maybe_history_policy: Option<HistoryPolicy>,
  pub maybe_is_archived: Option<bool>,
  pub maybe_moderation: Option<ChannelModeration>,
}

#[derive(Debug, Clone)]
pub struct ConversationMember {
  pub user: User,
  pub role: ChannelRole,
}

#[derive(Debug, Clone, Default)]
pub struct MembershipChanges {
  pub maybe_role: Option<ChannelRole>,
  pub maybe_notification_level: Option<NotificationLevel>,
  pub maybe_is_favorite: Option<bool>,
}

/// Stable key of a set of people: sorted IDs joined by `-`. Makes DM creation idempotent.
pub fn participants_key(users: &[UserId]) -> String {
  let mut ids: Vec<i64> = users.iter().map(|user| to_db(*user)).collect();
  ids.sort_unstable();
  ids.dedup();
  ids.iter().map(i64::to_string).collect::<Vec<_>>().join("-")
}

impl Database {
  /// Creates a channel in a space; the creator becomes channel admin.
  pub async fn create_channel(
    &self,
    new: &NewConversation<'_>,
    creator: UserId,
  ) -> Result<ConversationId, DbError> {
    let mut tx = self.pool().begin().await?;
    insert_conversation(&mut tx, new).await?;
    insert_membership(&mut tx, new.id, creator, ChannelRole::Admin).await?;
    tx.commit().await?;
    Ok(new.id)
  }

  /// Returns the existing DM / group DM of exactly these participants or creates it.
  /// The bool is `true` if it was created.
  pub async fn get_or_create_participants_conversation(
    &self,
    new: &NewConversation<'_>,
    participants: &[UserId],
  ) -> Result<(ConversationId, bool), DbError> {
    let mut tx = self.pool().begin().await?;
    let key = new.maybe_participants_key.as_deref();
    let existing = sqlx::query_scalar!("SELECT id FROM conversations WHERE participants_key = $1", key)
      .fetch_optional(&mut *tx)
      .await?;
    if let Some(id) = existing {
      return Ok((from_db(id)?, false));
    }
    insert_conversation(&mut tx, new).await?;
    for participant in participants {
      insert_membership(&mut tx, new.id, *participant, ChannelRole::Member).await?;
    }
    tx.commit().await?;
    Ok((new.id, true))
  }

  pub async fn get_conversation(
    &self,
    conversation: ConversationId,
    viewer: UserId,
  ) -> Result<Option<ConversationView>, DbError> {
    let filter = ConversationFilter {
      maybe_conversation: Some(conversation),
      ..ConversationFilter::default()
    };
    Ok(self.list_conversations(viewer, &filter).await?.into_iter().next())
  }

  pub async fn list_conversations(
    &self,
    viewer: UserId,
    filter: &ConversationFilter,
  ) -> Result<Vec<ConversationView>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT c.id, c.space_id, c.kind, c.name, c.topic, c.description, c.visibility, c.history_policy,
                c.category_id, c.archived_at IS NOT NULL AS "is_archived!", c.member_denied, c.guest_denied,
                m.role AS "role?", m.notification_level AS "notification_level?",
                m.is_favorite AS "is_favorite?", m.last_read_message_id,
                tc_snowflake_floor(m.joined_at) AS "joined_floor?"
         FROM conversations c
         LEFT JOIN memberships m ON m.conversation_id = c.id AND m.user_id = $1
         WHERE ($2::bigint IS NULL OR c.id = $2)
           AND ($3::bigint IS NULL OR c.space_id = $3)
           AND (NOT $4 OR m.user_id IS NOT NULL)
           AND (NOT $5 OR (c.kind = 'channel' AND c.visibility = 'public' AND c.archived_at IS NULL
                           AND m.user_id IS NULL))
         ORDER BY c.id"#,
      to_db(viewer),
      filter.maybe_conversation.map(to_db),
      filter.maybe_space.map(to_db),
      filter.only_joined,
      filter.only_joinable,
    )
    .fetch_all(self.pool())
    .await?;

    rows
      .into_iter()
      .map(|row| {
        let maybe_membership = match (
          row.role,
          row.notification_level,
          row.is_favorite,
          row.joined_floor,
        ) {
          (Some(role), Some(level), Some(is_favorite), Some(floor)) => Some(Membership {
            role: ChannelRole::from(role),
            notification_level: NotificationLevel::from(level),
            is_favorite,
            maybe_last_read_message_id: from_db_opt(row.last_read_message_id)?,
            joined_floor: from_db(floor)?,
          }),
          _ => None,
        };
        Ok(ConversationView {
          conversation: Conversation {
            id: from_db(row.id)?,
            maybe_space_id: from_db_opt(row.space_id)?,
            kind: ConversationKind::from(row.kind),
            maybe_name: row.name,
            maybe_topic: row.topic,
            maybe_description: row.description,
            visibility: ConversationVisibility::from(row.visibility),
            history_policy: HistoryPolicy::from(row.history_policy),
            maybe_category_id: from_db_opt(row.category_id)?,
            is_archived: row.is_archived,
            moderation: ChannelModeration {
              member_denied: permissions(row.member_denied),
              guest_denied: permissions(row.guest_denied),
            },
          },
          maybe_membership,
        })
      })
      .collect()
  }

  pub async fn update_conversation(
    &self,
    conversation: ConversationId,
    changes: &ConversationChanges<'_>,
  ) -> Result<(), DbError> {
    let member_denied = changes
      .maybe_moderation
      .as_ref()
      .map(|m| permission_names(&m.member_denied));
    let guest_denied = changes
      .maybe_moderation
      .as_ref()
      .map(|m| permission_names(&m.guest_denied));
    sqlx::query!(
      r#"UPDATE conversations SET
           name = COALESCE($2, name),
           topic = CASE WHEN $3::text IS NULL THEN topic ELSE NULLIF($3, '') END,
           description = CASE WHEN $4::text IS NULL THEN description ELSE NULLIF($4, '') END,
           visibility = COALESCE($5, visibility),
           category_id = CASE WHEN $6::bigint IS NOT NULL THEN $6 WHEN $7 THEN NULL ELSE category_id END,
           history_policy = COALESCE($8, history_policy),
           archived_at = CASE WHEN $9::boolean IS NULL THEN archived_at
                              WHEN $9 THEN COALESCE(archived_at, now()) ELSE NULL END,
           member_denied = COALESCE($10, member_denied),
           guest_denied = COALESCE($11, guest_denied)
         WHERE id = $1"#,
      to_db(conversation),
      changes.maybe_name,
      changes.maybe_topic,
      changes.maybe_description,
      changes
        .maybe_visibility
        .as_ref()
        .map(ConversationVisibility::as_str),
      changes.maybe_category.map(to_db),
      changes.clear_category,
      changes.maybe_history_policy.as_ref().map(HistoryPolicy::as_str),
      changes.maybe_is_archived,
      member_denied.as_deref(),
      guest_denied.as_deref(),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn list_conversation_members(
    &self,
    conversation: ConversationId,
  ) -> Result<Vec<ConversationMember>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT u.id, u.username, u.display_name, u.kind, u.created_at,
                u.deactivated_at IS NOT NULL AS "is_deactivated!", m.role
         FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.conversation_id = $1 ORDER BY u.display_name, u.id"#,
      to_db(conversation),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ConversationMember {
          user: User {
            id: from_db(row.id)?,
            maybe_username: row.username,
            display_name: row.display_name,
            kind: row.kind.into(),
            created_at: row.created_at,
            is_deactivated: row.is_deactivated,
          },
          role: ChannelRole::from(row.role),
        })
      })
      .collect()
  }

  pub async fn conversation_member_ids(&self, conversation: ConversationId) -> Result<Vec<UserId>, DbError> {
    let ids = sqlx::query_scalar!(
      "SELECT user_id FROM memberships WHERE conversation_id = $1",
      to_db(conversation)
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// Adds the user; keeps the existing membership if there is one. `true` if newly added.
  pub async fn add_conversation_member(
    &self,
    conversation: ConversationId,
    user: UserId,
    role: ChannelRole,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      r#"INSERT INTO memberships (conversation_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT DO NOTHING"#,
      to_db(conversation),
      to_db(user),
      role.as_str(),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// `true` if the user was a member.
  pub async fn remove_conversation_member(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "DELETE FROM memberships WHERE conversation_id = $1 AND user_id = $2",
      to_db(conversation),
      to_db(user),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  pub async fn update_membership(
    &self,
    conversation: ConversationId,
    user: UserId,
    changes: &MembershipChanges,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE memberships SET
           role = COALESCE($3, role),
           notification_level = COALESCE($4, notification_level),
           is_favorite = COALESCE($5, is_favorite)
         WHERE conversation_id = $1 AND user_id = $2"#,
      to_db(conversation),
      to_db(user),
      changes.maybe_role.as_ref().map(ChannelRole::as_str),
      changes
        .maybe_notification_level
        .as_ref()
        .map(NotificationLevel::as_str),
      changes.maybe_is_favorite,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  pub async fn count_channel_admins(&self, conversation: ConversationId) -> Result<i64, DbError> {
    let count = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM memberships WHERE conversation_id = $1 AND role = 'admin'"#,
      to_db(conversation),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(count)
  }

  /// Moves the read position forward (never back).
  pub async fn set_read_position(
    &self,
    conversation: ConversationId,
    user: UserId,
    message: MessageId,
  ) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE memberships SET last_read_message_id = GREATEST(COALESCE(last_read_message_id, 0), $3)
         WHERE conversation_id = $1 AND user_id = $2"#,
      to_db(conversation),
      to_db(user),
      to_db(message),
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Participants of DMs / group DMs, by conversation.
  pub async fn participants_of(
    &self,
    conversations: &[ConversationId],
  ) -> Result<HashMap<ConversationId, Vec<User>>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.conversation_id, u.id, u.username, u.display_name, u.kind, u.created_at,
                u.deactivated_at IS NOT NULL AS "is_deactivated!"
         FROM memberships m JOIN users u ON u.id = m.user_id
         JOIN conversations c ON c.id = m.conversation_id
         WHERE m.conversation_id = ANY($1) AND c.kind IN ('dm', 'group')
         ORDER BY u.display_name, u.id"#,
      &to_db_all(conversations),
    )
    .fetch_all(self.pool())
    .await?;
    let mut by_conversation: HashMap<ConversationId, Vec<User>> = HashMap::new();
    for row in rows {
      by_conversation
        .entry(from_db(row.conversation_id)?)
        .or_default()
        .push(User {
          id: from_db(row.id)?,
          maybe_username: row.username,
          display_name: row.display_name,
          kind: row.kind.into(),
          created_at: row.created_at,
          is_deactivated: row.is_deactivated,
        });
    }
    Ok(by_conversation)
  }

  /// Which of the conversations have at least one guest member ("external people" badge).
  pub async fn conversations_with_guests(
    &self,
    conversations: &[ConversationId],
  ) -> Result<HashSet<ConversationId>, DbError> {
    let ids = sqlx::query_scalar!(
      r#"SELECT DISTINCT m.conversation_id AS "id!"
         FROM memberships m JOIN users u ON u.id = m.user_id
         WHERE m.conversation_id = ANY($1) AND u.kind = 'guest'"#,
      &to_db_all(conversations),
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }

  /// IDs of all conversations the user is a member of.
  pub async fn conversation_ids_of_user(&self, user: UserId) -> Result<Vec<ConversationId>, DbError> {
    let ids = sqlx::query_scalar!(
      "SELECT conversation_id FROM memberships WHERE user_id = $1",
      to_db(user)
    )
    .fetch_all(self.pool())
    .await?;
    ids.into_iter().map(from_db).collect()
  }
}

async fn insert_conversation(tx: &mut sqlx::PgConnection, new: &NewConversation<'_>) -> Result<(), DbError> {
  sqlx::query!(
    r#"INSERT INTO conversations
         (id, space_id, kind, name, topic, description, visibility, history_policy, category_id, participants_key)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"#,
    to_db(new.id),
    new.maybe_space.map(to_db),
    new.kind.as_str(),
    new.maybe_name,
    new.maybe_topic,
    new.maybe_description,
    new.visibility.as_str(),
    new.history_policy.as_str(),
    new.maybe_category.map(to_db),
    new.maybe_participants_key,
  )
  .execute(tx)
  .await?;
  Ok(())
}

async fn insert_membership(
  tx: &mut sqlx::PgConnection,
  conversation: ConversationId,
  user: UserId,
  role: ChannelRole,
) -> Result<(), DbError> {
  sqlx::query!(
    "INSERT INTO memberships (conversation_id, user_id, role) VALUES ($1, $2, $3)",
    to_db(conversation),
    to_db(user),
    role.as_str(),
  )
  .execute(tx)
  .await?;
  Ok(())
}

fn permissions(names: Vec<String>) -> HashSet<Permission> {
  names.into_iter().map(Permission::from).collect()
}

fn permission_names(set: &HashSet<Permission>) -> Vec<String> {
  let mut names: Vec<String> = set.iter().map(|p| p.as_str().to_owned()).collect();
  names.sort();
  names
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  #[test]
  fn participants_key_ignores_order_and_duplicates() {
    assert_eq!(participants_key(&[user(30), user(4), user(30)]), "4-30");
    assert_eq!(
      participants_key(&[user(4), user(30)]),
      participants_key(&[user(30), user(4)])
    );
    assert_ne!(
      participants_key(&[user(4)]),
      participants_key(&[user(4), user(30)])
    );
  }
}
