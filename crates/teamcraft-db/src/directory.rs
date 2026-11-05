//! The directory's paged lists: people, the values of their filters, channels across spaces and
//! open invitations. Who may ask what is decided by the server's services.

use teamcraft_proto::{
  ConversationId, ConversationVisibility, InviteId, SpaceId, SpaceRole, UserId, UserKind,
};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};
use crate::user::{AccountFlags, User, escape_like, user_from_parts};

/// Filter and position of one page of people.
#[derive(Debug, Clone)]
pub struct DirectoryPeopleFilter<'a> {
  pub viewer: UserId,
  pub query: &'a str,
  /// Guests only find people who share a conversation with them.
  pub is_restricted_to_shared: bool,
  pub maybe_kind: Option<UserKind>,
  pub maybe_organization: Option<&'a str>,
  pub maybe_department: Option<&'a str>,
  pub maybe_title: Option<&'a str>,
  pub maybe_space: Option<SpaceId>,
  /// The name search and order go by is the username (`directory.shows_full_name` is off).
  pub hides_full_names: bool,
  /// Where the previous page ended: its `sort_name` and ID. Compared as given, with no lookup, so a
  /// position says nothing about rows the viewer may not see.
  pub maybe_after: Option<(&'a str, UserId)>,
  pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct DirectoryPerson {
  pub user: User,
  pub sort_name: String,
  pub maybe_title: Option<String>,
  pub maybe_department: Option<String>,
}

/// The values people can be filtered by.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DirectoryFilterValues {
  pub organizations: Vec<String>,
  pub departments: Vec<String>,
  pub titles: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DirectoryChannelFilter<'a> {
  pub viewer: UserId,
  pub query: &'a str,
  pub maybe_space: Option<SpaceId>,
  /// Where the previous page ended: its name and ID, compared as given (see
  /// [`DirectoryPeopleFilter::maybe_after`]).
  pub maybe_after: Option<(&'a str, ConversationId)>,
  pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct DirectoryChannel {
  pub id: ConversationId,
  pub space_id: SpaceId,
  pub space_name: String,
  pub name: String,
  pub maybe_topic: Option<String>,
  pub visibility: ConversationVisibility,
  pub member_count: i64,
  pub is_member: bool,
}

/// Which open invitations a page may hold: all of `managed_spaces`, and in
/// `guest_inviter_spaces` the guests `sponsor` invited.
#[derive(Debug, Clone)]
pub struct DirectoryInvitationFilter<'a> {
  pub sponsor: UserId,
  pub managed_spaces: &'a [SpaceId],
  pub guest_inviter_spaces: &'a [SpaceId],
  pub query: &'a str,
  pub maybe_space: Option<SpaceId>,
  pub maybe_after: Option<InviteId>,
  pub limit: i64,
}

#[derive(Debug, Clone)]
pub struct DirectoryInvitation {
  pub id: InviteId,
  pub space_id: SpaceId,
  pub space_name: String,
  pub is_space_archived: bool,
  pub email: String,
  pub role: SpaceRole,
  pub sponsor_user_id: UserId,
  pub maybe_expires_at: Option<OffsetDateTime>,
}

impl Database {
  /// One page of active people by name. Bots are never listed.
  pub async fn directory_people(
    &self,
    filter: &DirectoryPeopleFilter<'_>,
  ) -> Result<Vec<DirectoryPerson>, DbError> {
    let pattern = format!("%{}%", escape_like(filter.query));
    let rows = sqlx::query!(
      r#"SELECT u.id, u.username, u.display_name, u.kind, u.created_at, u.organization,
                u.deactivated_at IS NOT NULL AS "is_deactivated!", u.deleted_at IS NOT NULL AS "is_deleted!",
                p.title AS "title?", p.department AS "department?", shown.sort_name AS "sort_name!"
         FROM users u
         LEFT JOIN user_profiles p ON p.user_id = u.id
         CROSS JOIN LATERAL (
           SELECT CASE WHEN $11 THEN COALESCE(u.username, u.display_name) ELSE u.display_name END AS sort_name
         ) shown
         WHERE u.deactivated_at IS NULL
           AND u.kind <> 'bot'
           AND (shown.sort_name ILIKE $2 OR u.username ILIKE $2 OR p.title ILIKE $2 OR p.department ILIKE $2)
           AND ($3::text IS NULL OR u.kind = $3)
           AND ($4::text IS NULL OR u.organization = $4)
           AND ($5::text IS NULL OR p.department = $5)
           AND ($6::text IS NULL OR p.title = $6)
           AND ($7::bigint IS NULL OR EXISTS (
                 SELECT 1 FROM space_members m
                 WHERE m.space_id = $7 AND m.user_id = u.id
                   AND (m.expires_at IS NULL OR m.expires_at > now())))
           AND (NOT $8 OR EXISTS (
                 SELECT 1 FROM memberships a JOIN memberships b ON a.conversation_id = b.conversation_id
                 WHERE a.user_id = $1 AND b.user_id = u.id))
           AND ($9::bigint IS NULL OR (shown.sort_name, u.id) > ($12::text, $9))
         ORDER BY shown.sort_name, u.id
         LIMIT $10"#,
      to_db(filter.viewer),
      pattern,
      filter.maybe_kind.as_ref().map(UserKind::as_str),
      filter.maybe_organization,
      filter.maybe_department,
      filter.maybe_title,
      filter.maybe_space.map(to_db),
      filter.is_restricted_to_shared,
      filter.maybe_after.map(|(_, id)| to_db(id)),
      filter.limit,
      filter.hides_full_names,
      filter.maybe_after.map(|(name, _)| name),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|r| {
        Ok(DirectoryPerson {
          sort_name: r.sort_name,
          user: user_from_parts(
            r.id,
            r.username,
            r.display_name,
            r.kind,
            r.created_at,
            r.organization,
            AccountFlags {
              is_deactivated: r.is_deactivated,
              is_deleted: r.is_deleted,
            },
          )?,
          maybe_title: r.title,
          maybe_department: r.department,
        })
      })
      .collect()
  }

  /// The values of the people filters, each kind sorted and holding at most `limit` of the most
  /// common values.
  pub async fn directory_filter_values(
    &self,
    maybe_kind: Option<&UserKind>,
    maybe_space: Option<SpaceId>,
    limit: i64,
  ) -> Result<DirectoryFilterValues, DbError> {
    let rows = sqlx::query!(
      r#"SELECT ranked.facet AS "facet!", ranked.value AS "value!"
         FROM (
           SELECT x.facet, x.value,
                  row_number() OVER (PARTITION BY x.facet ORDER BY count(*) DESC, x.value) AS position
           FROM users u
           LEFT JOIN user_profiles p ON p.user_id = u.id
           CROSS JOIN LATERAL (VALUES ('organization', u.organization), ('department', p.department), ('title', p.title))
             AS x (facet, value)
           WHERE u.deactivated_at IS NULL
             AND u.kind <> 'bot'
             AND x.value IS NOT NULL AND x.value <> ''
             AND ($1::text IS NULL OR u.kind = $1)
             AND ($2::bigint IS NULL OR EXISTS (
                   SELECT 1 FROM space_members m
                   WHERE m.space_id = $2 AND m.user_id = u.id
                     AND (m.expires_at IS NULL OR m.expires_at > now())))
           GROUP BY x.facet, x.value
         ) ranked
         WHERE ranked.position <= $3
         ORDER BY ranked.value"#,
      maybe_kind.map(UserKind::as_str),
      maybe_space.map(to_db),
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    let mut values = DirectoryFilterValues::default();
    for row in rows {
      match row.facet.as_str() {
        "organization" => values.organizations.push(row.value),
        "department" => values.departments.push(row.value),
        _ => values.titles.push(row.value),
      }
    }
    Ok(values)
  }

  /// One page of unarchived channels by name: those the viewer is in, and the public ones of spaces
  /// the viewer is in.
  pub async fn directory_channels(
    &self,
    filter: &DirectoryChannelFilter<'_>,
  ) -> Result<Vec<DirectoryChannel>, DbError> {
    let pattern = format!("%{}%", escape_like(filter.query));
    let rows = sqlx::query!(
      r#"SELECT c.id, c.space_id AS "space_id!", s.name AS space_name, c.name AS "name!", c.topic, c.visibility,
                mine.user_id IS NOT NULL AS "is_member!",
                (SELECT count(*) FROM memberships n WHERE n.conversation_id = c.id) AS "member_count!"
         FROM conversations c
         JOIN spaces s ON s.id = c.space_id AND s.deleted_at IS NULL AND s.archived_at IS NULL
         LEFT JOIN memberships mine ON mine.conversation_id = c.id AND mine.user_id = $1
         WHERE c.kind = 'channel'
           AND c.deleted_at IS NULL
           AND c.archived_at IS NULL
           AND c.name IS NOT NULL
           AND (mine.user_id IS NOT NULL OR (c.visibility = 'public' AND EXISTS (
                 SELECT 1 FROM space_members m
                 WHERE m.space_id = c.space_id AND m.user_id = $1
                   AND (m.expires_at IS NULL OR m.expires_at > now()))))
           AND (c.name ILIKE $2 OR c.topic ILIKE $2)
           AND ($3::bigint IS NULL OR c.space_id = $3)
           AND ($4::bigint IS NULL OR (c.name, c.id) > ($6::text, $4))
         ORDER BY c.name, c.id
         LIMIT $5"#,
      to_db(filter.viewer),
      pattern,
      filter.maybe_space.map(to_db),
      filter.maybe_after.map(|(_, id)| to_db(id)),
      filter.limit,
      filter.maybe_after.map(|(name, _)| name),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|r| {
        Ok(DirectoryChannel {
          id: from_db(r.id)?,
          space_id: from_db(r.space_id)?,
          space_name: r.space_name,
          name: r.name,
          maybe_topic: r.topic,
          visibility: ConversationVisibility::from(r.visibility),
          member_count: r.member_count,
          is_member: r.is_member,
        })
      })
      .collect()
  }

  /// One page of open invitations, newest first.
  pub async fn directory_invitations(
    &self,
    filter: &DirectoryInvitationFilter<'_>,
  ) -> Result<Vec<DirectoryInvitation>, DbError> {
    let pattern = format!("%{}%", escape_like(filter.query));
    let rows = sqlx::query!(
      r#"SELECT i.id, i.space_id, s.name AS space_name, s.archived_at IS NOT NULL AS "is_space_archived!",
                i.email, i.space_role, i.sponsor_user_id, i.expires_at
         FROM invites i
         JOIN spaces s ON s.id = i.space_id AND s.deleted_at IS NULL
         WHERE i.accepted_at IS NULL
           AND (i.space_id = ANY($1::bigint[])
                OR (i.space_id = ANY($2::bigint[]) AND i.space_role = 'guest' AND i.sponsor_user_id = $3))
           AND i.email ILIKE $4
           AND ($5::bigint IS NULL OR i.space_id = $5)
           AND ($6::bigint IS NULL OR i.id < $6)
         ORDER BY i.id DESC
         LIMIT $7"#,
      &to_db_all(filter.managed_spaces),
      &to_db_all(filter.guest_inviter_spaces),
      to_db(filter.sponsor),
      pattern,
      filter.maybe_space.map(to_db),
      filter.maybe_after.map(to_db),
      filter.limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|r| {
        Ok(DirectoryInvitation {
          id: from_db(r.id)?,
          space_id: from_db(r.space_id)?,
          space_name: r.space_name,
          is_space_archived: r.is_space_archived,
          email: r.email,
          role: SpaceRole::from(r.space_role),
          sponsor_user_id: from_db(r.sponsor_user_id)?,
          maybe_expires_at: r.expires_at,
        })
      })
      .collect()
  }
}
