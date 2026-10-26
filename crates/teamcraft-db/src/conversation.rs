use std::collections::{HashMap, HashSet};

use teamcraft_proto::{
  CategoryId, ChannelModeration, ChannelRole, ConversationId, ConversationKind, ConversationVisibility,
  HistoryPolicy, IdempotencyKey, MessageId, NotificationLevel, Permission, SpaceId, UserId,
};

use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::idempotency::{Created, IdempotentKind, claim_key};
use crate::ids::{from_db, from_db_opt, to_db, to_db_all};
use crate::space::{StoredSpaceImage, stored_image};
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
  pub is_integration: bool,
  /// The channel's picture (channels in spaces only use it).
  pub maybe_image: Option<StoredSpaceImage>,
}

/// What [`Database::set_conversation_image`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageChange {
  /// Stored; the blob key of the previous picture, if there was a different one, can be deleted.
  Set { maybe_replaced_key: Option<String> },
  /// The channel doesn't exist (any more): nothing was stored.
  ChannelGone,
}

#[derive(Debug, Clone)]
pub struct Membership {
  pub role: ChannelRole,
  pub notification_level: NotificationLevel,
  pub is_favorite: bool,
  pub maybe_last_read_message_id: Option<MessageId>,
  pub maybe_muted_until: Option<OffsetDateTime>,
  /// The member never chose level or mute time here, so the space's setting applies.
  pub is_notification_inherited: bool,
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
  /// With `only_joinable`: private channels too, for workspace admins (admin.md §2.4).
  pub includes_private: bool,
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
  pub is_integration: bool,
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
  pub maybe_is_integration: Option<bool>,
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
    maybe_key: Option<&IdempotencyKey>,
  ) -> Result<Created<ConversationId>, DbError> {
    let mut tx = self.pool().begin().await?;
    if let Some(key) = maybe_key {
      let maybe_existing = claim_key(&mut tx, creator, IdempotentKind::Channel, key, to_db(new.id)).await?;
      if let Some(existing) = maybe_existing {
        return Ok(Created {
          id: from_db(existing)?,
          is_new: false,
        });
      }
    }
    insert_conversation(&mut tx, new).await?;
    insert_membership(&mut tx, new.id, creator, ChannelRole::Admin).await?;
    tx.commit().await?;
    Ok(Created {
      id: new.id,
      is_new: true,
    })
  }

  /// Returns the existing DM / group DM of exactly these participants or creates it.
  /// The bool is `true` if it was created.
  pub async fn get_or_create_participants_conversation(
    &self,
    new: &NewConversation<'_>,
    participants: &[UserId],
  ) -> Result<(ConversationId, bool), DbError> {
    let key = new.maybe_participants_key.as_deref();
    if let Some(id) = self.participants_conversation(key).await? {
      return Ok((id, false));
    }
    match self.insert_participants_conversation(new, participants).await {
      Ok(()) => Ok((new.id, true)),
      // Two requests for the same people at once (two devices making the notes to self, a retry
      // after a lost response): the unique key lets one insert win; the other returns its
      // conversation instead of failing (issue #643).
      Err(DbError::Query(sqlx::Error::Database(error))) if error.is_unique_violation() => {
        let maybe_winner = self.participants_conversation(key).await?;
        maybe_winner
          .map(|id| (id, false))
          .ok_or_else(|| DbError::InvalidStoredValue("participants conversation vanished".to_owned()))
      },
      Err(error) => Err(error),
    }
  }

  async fn participants_conversation(&self, key: Option<&str>) -> Result<Option<ConversationId>, DbError> {
    let existing = sqlx::query_scalar!("SELECT id FROM conversations WHERE participants_key = $1", key)
      .fetch_optional(self.pool())
      .await?;
    from_db_opt(existing)
  }

  async fn insert_participants_conversation(
    &self,
    new: &NewConversation<'_>,
    participants: &[UserId],
  ) -> Result<(), DbError> {
    let mut tx = self.pool().begin().await?;
    insert_conversation(&mut tx, new).await?;
    for participant in participants {
      insert_membership(&mut tx, new.id, *participant, ChannelRole::Member).await?;
    }
    tx.commit().await?;
    Ok(())
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
                c.category_id, c.archived_at IS NOT NULL AS "is_archived!", c.is_integration, c.member_denied, c.guest_denied,
                c.image_key, c.image_version, c.image_size_bytes,
                m.role AS "role?", m.notification_level AS "notification_level?",
                m.is_favorite AS "is_favorite?", m.last_read_message_id, m.muted_until,
                NOT m.notification_overridden AS "is_inherited?",
                tc_snowflake_floor(m.joined_at) AS "joined_floor?"
         FROM conversations c
         LEFT JOIN memberships m ON m.conversation_id = c.id AND m.user_id = $1
         WHERE ($2::bigint IS NULL OR c.id = $2)
           AND ($3::bigint IS NULL OR c.space_id = $3)
           AND (NOT $4 OR m.user_id IS NOT NULL)
           AND (NOT $5 OR (c.kind = 'channel' AND (c.visibility = 'public' OR $6)
                           AND c.archived_at IS NULL AND m.user_id IS NULL))
         ORDER BY c.id"#,
      to_db(viewer),
      filter.maybe_conversation.map(to_db),
      filter.maybe_space.map(to_db),
      filter.only_joined,
      filter.only_joinable,
      filter.includes_private,
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
            is_notification_inherited: row.is_inherited.unwrap_or(false),
            role: ChannelRole::from(role),
            notification_level: NotificationLevel::from(level),
            is_favorite,
            maybe_last_read_message_id: from_db_opt(row.last_read_message_id)?,
            maybe_muted_until: row.muted_until,
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
            is_integration: row.is_integration,
            maybe_image: stored_image(row.image_key, row.image_version, row.image_size_bytes)?,
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

  /// Makes `image` the channel's picture (`None` removes it). Says which blob key it replaced,
  /// which nothing refers to any more, or that the channel is gone (deleted while the picture was
  /// uploading), so the caller deletes the object it stored.
  pub async fn set_conversation_image(
    &self,
    conversation: ConversationId,
    maybe_image: Option<&StoredSpaceImage>,
  ) -> Result<ImageChange, DbError> {
    let mut tx = self.pool().begin().await?;
    let maybe_previous = sqlx::query_scalar!(
      "SELECT image_key FROM conversations WHERE id = $1 FOR UPDATE",
      to_db(conversation),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(maybe_previous) = maybe_previous else {
      return Ok(ImageChange::ChannelGone);
    };
    let maybe_size = maybe_image
      .map(|image| i64::try_from(image.size_bytes))
      .transpose()
      .map_err(|_| DbError::InvalidStoredValue("channel image size".into()))?;
    sqlx::query!(
      "UPDATE conversations SET image_key = $2, image_version = $3, image_size_bytes = $4 WHERE id = $1",
      to_db(conversation),
      maybe_image.map(|image| image.key.as_str()),
      maybe_image.map(|image| image.version.as_str()),
      maybe_size,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ImageChange::Set {
      maybe_replaced_key: maybe_previous
        .filter(|previous| maybe_image.is_none_or(|image| image.key != *previous)),
    })
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
           guest_denied = COALESCE($11, guest_denied),
           is_integration = COALESCE($12, is_integration)
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
      changes.maybe_is_integration,
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
      r#"SELECT u.id, u.username, u.display_name, u.kind, u.created_at, u.organization,
                u.deactivated_at IS NOT NULL AS "is_deactivated!", u.deleted_at IS NOT NULL AS "is_deleted!", m.role
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
            is_deleted: row.is_deleted,
            maybe_organization: row.organization,
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

  /// The members of every conversation in the space, by conversation; with `maybe_user` only that
  /// user's conversations there. Conversations without members are left out.
  pub async fn space_conversation_members(
    &self,
    space: SpaceId,
    maybe_user: Option<UserId>,
  ) -> Result<HashMap<ConversationId, Vec<UserId>>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT m.conversation_id, m.user_id
         FROM memberships m
         JOIN conversations c ON c.id = m.conversation_id
         WHERE c.space_id = $1 AND ($2::bigint IS NULL OR m.user_id = $2)"#,
      to_db(space),
      maybe_user.map(to_db),
    )
    .fetch_all(self.pool())
    .await?;
    let mut members: HashMap<ConversationId, Vec<UserId>> = HashMap::new();
    for row in rows {
      members
        .entry(from_db(row.conversation_id)?)
        .or_default()
        .push(from_db(row.user_id)?);
    }
    Ok(members)
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
           notification_overridden = notification_overridden OR $4::text IS NOT NULL,
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

  /// Moves a channel to another space and files it under `maybe_category` (of that space) or none.
  pub async fn move_conversation_to_space(
    &self,
    conversation: ConversationId,
    space: SpaceId,
    maybe_category: Option<CategoryId>,
  ) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE conversations SET space_id = $2, category_id = $3 WHERE id = $1 AND kind = 'channel'",
      to_db(conversation),
      to_db(space),
      maybe_category.map(to_db),
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

  /// Participants of DMs / group DMs, by conversation. A DM's participants are the two people
  /// (or the one, for notes to self) it was opened for, from its [`participants_key`]: a
  /// deactivated user loses their memberships, yet the DM with them stays the DM with them and
  /// never turns into a conversation of the user alone (issue #643). A group DM's are its members.
  pub async fn participants_of(
    &self,
    conversations: &[ConversationId],
  ) -> Result<HashMap<ConversationId, Vec<User>>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT c.id AS conversation_id, u.id, u.username, u.display_name, u.kind, u.created_at, u.organization,
                u.deactivated_at IS NOT NULL AS "is_deactivated!", u.deleted_at IS NOT NULL AS "is_deleted!"
         FROM conversations c JOIN users u
           ON (c.kind = 'dm' AND u.id = ANY(string_to_array(c.participants_key, '-')::bigint[]))
           OR (c.kind = 'group'
               AND EXISTS (SELECT 1 FROM memberships m WHERE m.conversation_id = c.id AND m.user_id = u.id))
         WHERE c.id = ANY($1) AND c.kind IN ('dm', 'group')
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
          is_deleted: row.is_deleted,
          maybe_organization: row.organization,
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

pub(crate) async fn insert_conversation(
  tx: &mut sqlx::PgConnection,
  new: &NewConversation<'_>,
) -> Result<(), DbError> {
  sqlx::query!(
    r#"INSERT INTO conversations
         (id, space_id, kind, name, topic, description, visibility, history_policy, category_id, participants_key,
          is_integration)
       VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)"#,
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
    new.is_integration,
  )
  .execute(tx)
  .await?;
  Ok(())
}

pub(crate) async fn insert_membership(
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
