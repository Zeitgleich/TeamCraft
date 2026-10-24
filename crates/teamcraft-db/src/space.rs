use teamcraft_proto::{
  CategoryId, IdempotencyKey, JoinPolicy, NotificationLevel, SpaceId, SpaceRole, UserId,
};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::idempotency::{Created, IdempotentKind, claim_key};
use crate::ids::{from_db, from_db_opt, to_db};
use crate::notification::StoredSpaceNotifications;
use crate::user::User;

#[derive(Debug, Clone)]
pub struct Space {
  pub id: SpaceId,
  pub name: String,
  pub maybe_icon: Option<String>,
  pub maybe_color: Option<String>,
  pub maybe_description: Option<String>,
  pub join_policy: JoinPolicy,
  pub is_archived: bool,
  /// Optional brand tag: a default theming hint (docs/branding.md §7).
  pub maybe_brand_id: Option<String>,
  pub maybe_image: Option<StoredSpaceImage>,
}

/// The space's picture: where it is in the blob store, and the version clients cache it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSpaceImage {
  pub key: String,
  pub version: String,
  pub size_bytes: u64,
}

/// Which spaces to list for a viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceListing {
  /// Spaces the viewer is a member of.
  Mine,
  /// Plus open, unarchived spaces they could join.
  MineAndJoinable,
  /// Every space (instance admins).
  All,
}

/// A space and the viewer's role in it (`None`: not a member).
#[derive(Debug, Clone)]
pub struct SpaceView {
  pub space: Space,
  pub maybe_role: Option<SpaceRole>,
  /// The viewer's notification setting for the space (default for non-members).
  pub notifications: StoredSpaceNotifications,
  /// Members whose membership hasn't expired.
  pub member_count: u32,
}

#[derive(Debug, Clone)]
pub struct NewSpace<'a> {
  pub id: SpaceId,
  pub name: &'a str,
  pub maybe_icon: Option<&'a str>,
  pub maybe_color: Option<&'a str>,
  pub maybe_description: Option<&'a str>,
  pub join_policy: JoinPolicy,
  pub creator: UserId,
  pub maybe_idempotency_key: Option<IdempotencyKey>,
}

/// Fields to change; `None` keeps the stored value. An empty string clears icon, color, description.
#[derive(Debug, Clone, Default)]
pub struct SpaceChanges<'a> {
  pub maybe_name: Option<&'a str>,
  pub maybe_icon: Option<&'a str>,
  pub maybe_color: Option<&'a str>,
  pub maybe_description: Option<&'a str>,
  pub maybe_join_policy: Option<JoinPolicy>,
  pub maybe_is_archived: Option<bool>,
  /// An empty string clears the brand tag.
  pub maybe_brand_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct SpaceMember {
  pub user: User,
  pub role: SpaceRole,
  pub maybe_expires_at: Option<OffsetDateTime>,
  pub maybe_sponsor: Option<UserId>,
}

/// What to do with a member's expiry when their membership is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExpiryChange {
  Keep,
  Set(OffsetDateTime),
  Clear,
}

/// A guest whose membership ran out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredGuest {
  pub space: SpaceId,
  pub user: UserId,
  pub maybe_sponsor: Option<UserId>,
}

#[derive(Debug, Clone)]
pub struct NewSpaceMember {
  pub space: SpaceId,
  pub user: UserId,
  pub role: SpaceRole,
  pub expiry: ExpiryChange,
  /// `None` keeps the stored sponsor.
  pub maybe_sponsor: Option<UserId>,
}

#[derive(Debug, Clone)]
pub struct Category {
  pub id: CategoryId,
  pub space_id: SpaceId,
  pub name: String,
  pub position: i32,
}

impl Database {
  /// Creates the space; the creator becomes its first space admin. A repeated idempotency key
  /// creates nothing and returns the first space.
  pub async fn create_space(&self, space: &NewSpace<'_>) -> Result<Created<SpaceId>, DbError> {
    let mut tx = self.pool().begin().await?;
    if let Some(key) = &space.maybe_idempotency_key {
      let maybe_existing = claim_key(
        &mut tx,
        space.creator,
        IdempotentKind::Space,
        key,
        to_db(space.id),
      )
      .await?;
      if let Some(existing) = maybe_existing {
        return Ok(Created {
          id: from_db(existing)?,
          is_new: false,
        });
      }
    }
    sqlx::query!(
      "INSERT INTO spaces (id, name, icon_ref, color, description, join_policy) VALUES ($1, $2, $3, $4, $5, $6)",
      to_db(space.id),
      space.name,
      space.maybe_icon,
      space.maybe_color,
      space.maybe_description,
      space.join_policy.as_str(),
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
      "INSERT INTO space_members (space_id, user_id, role) VALUES ($1, $2, $3)",
      to_db(space.id),
      to_db(space.creator),
      SpaceRole::SpaceAdmin.as_str(),
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Created {
      id: space.id,
      is_new: true,
    })
  }

  /// One space with the viewer's role.
  pub async fn get_space(&self, space: SpaceId, viewer: UserId) -> Result<Option<SpaceView>, DbError> {
    Ok(
      self
        .list_spaces(viewer, Some(space), SpaceListing::Mine)
        .await?
        .into_iter()
        .next(),
    )
  }

  pub async fn list_spaces_for_user(
    &self,
    viewer: UserId,
    listing: SpaceListing,
  ) -> Result<Vec<SpaceView>, DbError> {
    self.list_spaces(viewer, None, listing).await
  }

  async fn list_spaces(
    &self,
    viewer: UserId,
    maybe_space: Option<SpaceId>,
    listing: SpaceListing,
  ) -> Result<Vec<SpaceView>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT s.id, s.name, s.icon_ref, s.color, s.description, s.join_policy, s.brand_id,
                s.image_key, s.image_version, s.image_size_bytes,
                s.archived_at IS NOT NULL AS "is_archived!", m.role AS "role?",
                m.notification_level AS "notification_level?", m.muted_until AS "muted_until?",
                (SELECT count(*) FROM space_members c
                  WHERE c.space_id = s.id AND (c.expires_at IS NULL OR c.expires_at > now()))
                  AS "member_count!"
         FROM spaces s
         LEFT JOIN space_members m ON m.space_id = s.id AND m.user_id = $1
              AND (m.expires_at IS NULL OR m.expires_at > now())
         WHERE ($2::bigint IS NULL OR s.id = $2)
           AND ($2::bigint IS NOT NULL OR $4 OR m.user_id IS NOT NULL
                OR ($3 AND s.join_policy = 'open' AND s.archived_at IS NULL))
         ORDER BY s.name, s.id"#,
      to_db(viewer),
      maybe_space.map(to_db),
      listing == SpaceListing::MineAndJoinable,
      listing == SpaceListing::All,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(SpaceView {
          space: Space {
            id: from_db(row.id)?,
            name: row.name,
            maybe_icon: row.icon_ref,
            maybe_color: row.color,
            maybe_description: row.description,
            join_policy: JoinPolicy::from(row.join_policy),
            is_archived: row.is_archived,
            maybe_brand_id: row.brand_id,
            maybe_image: stored_image(row.image_key, row.image_version, row.image_size_bytes)?,
          },
          maybe_role: row.role.map(SpaceRole::from),
          notifications: StoredSpaceNotifications {
            maybe_level: row.notification_level.map(NotificationLevel::from),
            maybe_muted_until: row.muted_until,
          },
          member_count: u32::try_from(row.member_count).unwrap_or(u32::MAX),
        })
      })
      .collect()
  }

  pub async fn update_space(&self, space: SpaceId, changes: &SpaceChanges<'_>) -> Result<(), DbError> {
    sqlx::query!(
      r#"UPDATE spaces SET
           name = COALESCE($2, name),
           icon_ref = CASE WHEN $3::text IS NULL THEN icon_ref ELSE NULLIF($3, '') END,
           color = CASE WHEN $4::text IS NULL THEN color ELSE NULLIF($4, '') END,
           description = CASE WHEN $5::text IS NULL THEN description ELSE NULLIF($5, '') END,
           join_policy = COALESCE($6, join_policy),
           archived_at = CASE WHEN $7::boolean IS NULL THEN archived_at
                              WHEN $7 THEN COALESCE(archived_at, now()) ELSE NULL END,
           brand_id = CASE WHEN $8::text IS NULL THEN brand_id ELSE NULLIF($8, '') END
         WHERE id = $1"#,
      to_db(space),
      changes.maybe_name,
      changes.maybe_icon,
      changes.maybe_color,
      changes.maybe_description,
      changes.maybe_join_policy.as_ref().map(JoinPolicy::as_str),
      changes.maybe_is_archived,
      changes.maybe_brand_id,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Makes `image` the space's picture (`None` removes it). Returns the blob key of the picture it
  /// replaced, which nothing refers to any more; `Ok(None)` also when the space doesn't exist.
  pub async fn set_space_image(
    &self,
    space: SpaceId,
    maybe_image: Option<&StoredSpaceImage>,
  ) -> Result<Option<String>, DbError> {
    let mut tx = self.pool().begin().await?;
    let maybe_previous = sqlx::query_scalar!(
      "SELECT image_key FROM spaces WHERE id = $1 FOR UPDATE",
      to_db(space),
    )
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    let maybe_size = maybe_image
      .map(|image| i64::try_from(image.size_bytes))
      .transpose()
      .map_err(|_| DbError::InvalidStoredValue("space image size".into()))?;
    sqlx::query!(
      "UPDATE spaces SET image_key = $2, image_version = $3, image_size_bytes = $4 WHERE id = $1",
      to_db(space),
      maybe_image.map(|image| image.key.as_str()),
      maybe_image.map(|image| image.version.as_str()),
      maybe_size,
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(maybe_previous.filter(|previous| maybe_image.is_none_or(|image| image.key != *previous)))
  }

  /// The user's role in the space, `None` if not a member (or the guest membership expired).
  pub async fn space_role(&self, space: SpaceId, user: UserId) -> Result<Option<SpaceRole>, DbError> {
    let role = sqlx::query_scalar!(
      "SELECT role FROM space_members WHERE space_id = $1 AND user_id = $2 AND (expires_at IS NULL OR expires_at > now())",
      to_db(space),
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(role.map(SpaceRole::from))
  }

  pub async fn list_space_members(&self, space: SpaceId) -> Result<Vec<SpaceMember>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT u.id, u.username, u.display_name, u.kind, u.created_at, u.organization,
                u.deactivated_at IS NOT NULL AS "is_deactivated!", m.role, m.expires_at, m.sponsor_user_id
         FROM space_members m JOIN users u ON u.id = m.user_id
         WHERE m.space_id = $1 AND (m.expires_at IS NULL OR m.expires_at > now())
         ORDER BY u.display_name, u.id"#,
      to_db(space),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(SpaceMember {
          user: User {
            id: from_db(row.id)?,
            maybe_username: row.username,
            display_name: row.display_name,
            kind: row.kind.into(),
            created_at: row.created_at,
            is_deactivated: row.is_deactivated,
            maybe_organization: row.organization,
          },
          role: SpaceRole::from(row.role),
          maybe_expires_at: row.expires_at,
          maybe_sponsor: from_db_opt(row.sponsor_user_id)?,
        })
      })
      .collect()
  }

  /// Adds the user, or updates role, expiry and sponsor if they are already a member.
  pub async fn upsert_space_member(&self, member: &NewSpaceMember) -> Result<(), DbError> {
    sqlx::query!(
      r#"INSERT INTO space_members (space_id, user_id, role, expires_at, sponsor_user_id)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (space_id, user_id) DO UPDATE
           SET role = EXCLUDED.role,
               expires_at = CASE WHEN $6 THEN EXCLUDED.expires_at ELSE space_members.expires_at END,
               sponsor_user_id = COALESCE(EXCLUDED.sponsor_user_id, space_members.sponsor_user_id)"#,
      to_db(member.space),
      to_db(member.user),
      member.role.as_str(),
      match member.expiry {
        ExpiryChange::Set(at) => Some(at),
        ExpiryChange::Keep | ExpiryChange::Clear => None,
      },
      member.maybe_sponsor.map(to_db),
      member.expiry != ExpiryChange::Keep,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Guest memberships whose expiry has passed at `now`, oldest first.
  pub async fn list_expired_guests(
    &self,
    now: OffsetDateTime,
    limit: i64,
  ) -> Result<Vec<ExpiredGuest>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT space_id, user_id, sponsor_user_id FROM space_members
         WHERE role = 'guest' AND expires_at IS NOT NULL AND expires_at <= $1
         ORDER BY expires_at LIMIT $2"#,
      now,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(ExpiredGuest {
          space: from_db(row.space_id)?,
          user: from_db(row.user_id)?,
          maybe_sponsor: from_db_opt(row.sponsor_user_id)?,
        })
      })
      .collect()
  }

  /// Expiry and sponsor of a membership.
  pub async fn space_member_terms(
    &self,
    space: SpaceId,
    user: UserId,
  ) -> Result<Option<(Option<OffsetDateTime>, Option<UserId>)>, DbError> {
    let row = sqlx::query!(
      "SELECT expires_at, sponsor_user_id FROM space_members WHERE space_id = $1 AND user_id = $2",
      to_db(space),
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| Ok((row.expires_at, from_db_opt(row.sponsor_user_id)?)))
      .transpose()
  }

  /// Removes the user from the space and from all of its channels. Returns the channels they left.
  pub async fn remove_space_member(
    &self,
    space: SpaceId,
    user: UserId,
  ) -> Result<Vec<teamcraft_proto::ConversationId>, DbError> {
    let mut tx = self.pool().begin().await?;
    let left = sqlx::query_scalar!(
      r#"DELETE FROM memberships USING conversations c
         WHERE memberships.conversation_id = c.id AND c.space_id = $1 AND memberships.user_id = $2
         RETURNING c.id AS "id!""#,
      to_db(space),
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query!(
      "DELETE FROM space_members WHERE space_id = $1 AND user_id = $2",
      to_db(space),
      to_db(user)
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    left.into_iter().map(from_db).collect()
  }

  /// Number of space admins; the last one can't be removed or demoted.
  pub async fn count_space_admins(&self, space: SpaceId) -> Result<i64, DbError> {
    let count = sqlx::query_scalar!(
      r#"SELECT count(*) AS "count!" FROM space_members WHERE space_id = $1 AND role = 'space_admin'"#,
      to_db(space),
    )
    .fetch_one(self.pool())
    .await?;
    Ok(count)
  }

  pub async fn list_categories(&self, space: SpaceId) -> Result<Vec<Category>, DbError> {
    let rows = sqlx::query!(
      "SELECT id, space_id, name, position FROM channel_categories WHERE space_id = $1 ORDER BY position, id",
      to_db(space),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(Category {
          id: from_db(row.id)?,
          space_id: from_db(row.space_id)?,
          name: row.name,
          position: row.position,
        })
      })
      .collect()
  }

  pub async fn get_category(&self, category: CategoryId) -> Result<Option<Category>, DbError> {
    let row = sqlx::query!(
      "SELECT id, space_id, name, position FROM channel_categories WHERE id = $1",
      to_db(category),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(Category {
          id: from_db(row.id)?,
          space_id: from_db(row.space_id)?,
          name: row.name,
          position: row.position,
        })
      })
      .transpose()
  }

  pub async fn create_category(
    &self,
    id: CategoryId,
    space: SpaceId,
    name: &str,
    maybe_position: Option<i32>,
  ) -> Result<Category, DbError> {
    let row = sqlx::query!(
      r#"INSERT INTO channel_categories (id, space_id, name, position)
         VALUES ($1, $2, $3,
                 COALESCE($4, (SELECT COALESCE(max(position) + 1, 0) FROM channel_categories WHERE space_id = $2)))
         RETURNING id, space_id, name, position"#,
      to_db(id),
      to_db(space),
      name,
      maybe_position,
    )
    .fetch_one(self.pool())
    .await?;
    Ok(Category {
      id: from_db(row.id)?,
      space_id: from_db(row.space_id)?,
      name: row.name,
      position: row.position,
    })
  }

  pub async fn update_category(
    &self,
    category: CategoryId,
    maybe_name: Option<&str>,
    maybe_position: Option<i32>,
  ) -> Result<(), DbError> {
    sqlx::query!(
      "UPDATE channel_categories SET name = COALESCE($2, name), position = COALESCE($3, position) WHERE id = $1",
      to_db(category),
      maybe_name,
      maybe_position,
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Channels of the category move to "uncategorized".
  pub async fn delete_category(&self, category: CategoryId) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM channel_categories WHERE id = $1", to_db(category))
      .execute(self.pool())
      .await?;
    Ok(())
  }
}

/// The three image columns, set together (a table constraint).
fn stored_image(
  maybe_key: Option<String>,
  maybe_version: Option<String>,
  maybe_size: Option<i64>,
) -> Result<Option<StoredSpaceImage>, DbError> {
  match (maybe_key, maybe_version, maybe_size) {
    (Some(key), Some(version), Some(size)) => Ok(Some(StoredSpaceImage {
      key,
      version,
      size_bytes: u64::try_from(size).map_err(|_| DbError::InvalidStoredValue("space image size".into()))?,
    })),
    _ => Ok(None),
  }
}
