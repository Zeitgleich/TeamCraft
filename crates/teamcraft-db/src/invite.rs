use teamcraft_proto::{ConversationId, InviteId, SpaceId, SpaceRole, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, from_db_all, to_db, to_db_all};

#[derive(Debug, Clone)]
pub struct Invite {
  pub id: InviteId,
  pub space_id: SpaceId,
  pub email: String,
  pub role: SpaceRole,
  pub channel_ids: Vec<ConversationId>,
  pub sponsor_user_id: UserId,
  pub maybe_expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone)]
pub struct NewInvite<'a> {
  pub id: InviteId,
  pub space_id: SpaceId,
  pub email: &'a str,
  pub role: SpaceRole,
  pub channel_ids: &'a [ConversationId],
  pub sponsor: UserId,
  pub maybe_expires_at: Option<OffsetDateTime>,
}

/// What accepting an invite changed for the user.
#[derive(Debug, Clone)]
pub struct AppliedInvite {
  pub space_id: SpaceId,
  pub joined_channels: Vec<ConversationId>,
}

impl Database {
  pub async fn create_invite(&self, new: &NewInvite<'_>) -> Result<Invite, DbError> {
    sqlx::query!(
      r#"INSERT INTO invites (id, space_id, email, space_role, channel_ids, sponsor_user_id, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)"#,
      to_db(new.id),
      to_db(new.space_id),
      new.email.to_lowercase(),
      new.role.as_str(),
      &to_db_all(new.channel_ids),
      to_db(new.sponsor),
      new.maybe_expires_at,
    )
    .execute(self.pool())
    .await?;
    Ok(Invite {
      id: new.id,
      space_id: new.space_id,
      email: new.email.to_lowercase(),
      role: new.role.clone(),
      channel_ids: new.channel_ids.to_vec(),
      sponsor_user_id: new.sponsor,
      maybe_expires_at: new.maybe_expires_at,
    })
  }

  pub async fn list_pending_invites(&self, space: SpaceId) -> Result<Vec<Invite>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT id, space_id, email, space_role, channel_ids, sponsor_user_id, expires_at
         FROM invites WHERE space_id = $1 AND accepted_at IS NULL ORDER BY id"#,
      to_db(space),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(Invite {
          id: from_db(row.id)?,
          space_id: from_db(row.space_id)?,
          email: row.email,
          role: SpaceRole::from(row.space_role),
          channel_ids: from_db_all(&row.channel_ids)?,
          sponsor_user_id: from_db(row.sponsor_user_id)?,
          maybe_expires_at: row.expires_at,
        })
      })
      .collect()
  }

  pub async fn get_invite(&self, invite: InviteId) -> Result<Option<Invite>, DbError> {
    let row = sqlx::query!(
      r#"SELECT id, space_id, email, space_role, channel_ids, sponsor_user_id, expires_at
         FROM invites WHERE id = $1 AND accepted_at IS NULL"#,
      to_db(invite),
    )
    .fetch_optional(self.pool())
    .await?;
    row
      .map(|row| {
        Ok(Invite {
          id: from_db(row.id)?,
          space_id: from_db(row.space_id)?,
          email: row.email,
          role: SpaceRole::from(row.space_role),
          channel_ids: from_db_all(&row.channel_ids)?,
          sponsor_user_id: from_db(row.sponsor_user_id)?,
          maybe_expires_at: row.expires_at,
        })
      })
      .transpose()
  }

  pub async fn delete_invite(&self, invite: InviteId) -> Result<(), DbError> {
    sqlx::query!(
      "DELETE FROM invites WHERE id = $1 AND accepted_at IS NULL",
      to_db(invite)
    )
    .execute(self.pool())
    .await?;
    Ok(())
  }

  /// Applies all pending invites for a (verified) email to the user: space membership with the
  /// invited role and membership in the listed channels of that space. Guest memberships expire
  /// when the invite says so. Idempotent; returns what was applied.
  pub async fn accept_pending_invites(
    &self,
    user: UserId,
    email: &str,
    guest_default_expiry_secs: Option<f64>,
  ) -> Result<Vec<AppliedInvite>, DbError> {
    let mut tx = self.pool().begin().await?;
    let invites = sqlx::query!(
      r#"UPDATE invites SET accepted_at = now(), accepted_by = $2
         WHERE lower(email) = lower($1) AND accepted_at IS NULL AND (expires_at IS NULL OR expires_at > now())
         RETURNING space_id, space_role, channel_ids, sponsor_user_id, expires_at"#,
      email,
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;

    let mut applied = Vec::new();
    for invite in invites {
      sqlx::query!(
        r#"INSERT INTO space_members (space_id, user_id, role, expires_at, sponsor_user_id)
           VALUES ($1, $2, $3,
                   COALESCE($4, CASE WHEN $3 = 'guest' AND $6::float8 IS NOT NULL
                                     THEN now() + make_interval(secs => $6) END),
                   $5)
           ON CONFLICT (space_id, user_id) DO NOTHING"#,
        invite.space_id,
        to_db(user),
        invite.space_role,
        invite.expires_at,
        invite.sponsor_user_id,
        guest_default_expiry_secs,
      )
      .execute(&mut *tx)
      .await?;
      let joined = sqlx::query_scalar!(
        r#"INSERT INTO memberships (conversation_id, user_id, role)
           SELECT c.id, $2, 'member' FROM conversations c
           WHERE c.id = ANY($1) AND c.space_id = $3 AND c.kind = 'channel'
           ON CONFLICT DO NOTHING RETURNING conversation_id"#,
        &invite.channel_ids,
        to_db(user),
        invite.space_id,
      )
      .fetch_all(&mut *tx)
      .await?;
      applied.push(AppliedInvite {
        space_id: from_db(invite.space_id)?,
        joined_channels: from_db_all(&joined)?,
      });
    }
    tx.commit().await?;
    Ok(applied)
  }

  /// Gives a new user the instance's default spaces (as members) and, of those spaces, the listed
  /// public channels. Archived spaces and channels and private channels are skipped. Claims
  /// `default_memberships_at`, so it applies at most once per user: later calls return nothing.
  pub async fn apply_default_memberships(
    &self,
    user: UserId,
    spaces: &[SpaceId],
    channels: &[ConversationId],
  ) -> Result<Vec<AppliedInvite>, DbError> {
    let mut tx = self.pool().begin().await?;
    let claimed = sqlx::query!(
      "UPDATE users SET default_memberships_at = now()
       WHERE id = $1 AND default_memberships_at IS NULL AND deactivated_at IS NULL",
      to_db(user),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if claimed == 0 || spaces.is_empty() {
      tx.commit().await?;
      return Ok(Vec::new());
    }
    let joined_spaces = sqlx::query_scalar!(
      r#"INSERT INTO space_members (space_id, user_id, role)
         SELECT s.id, $2, 'member' FROM spaces s
         WHERE s.id = ANY($1) AND s.archived_at IS NULL
         ON CONFLICT (space_id, user_id) DO NOTHING RETURNING space_id"#,
      &to_db_all(spaces),
      to_db(user),
    )
    .fetch_all(&mut *tx)
    .await?;
    let mut applied = Vec::new();
    for space in joined_spaces {
      let joined = sqlx::query_scalar!(
        r#"INSERT INTO memberships (conversation_id, user_id, role)
           SELECT c.id, $2, 'member' FROM conversations c
           WHERE c.space_id = $1 AND c.kind = 'channel' AND c.visibility = 'public'
             AND c.archived_at IS NULL AND c.id = ANY($3)
           ON CONFLICT DO NOTHING RETURNING conversation_id"#,
        space,
        to_db(user),
        &to_db_all(channels),
      )
      .fetch_all(&mut *tx)
      .await?;
      applied.push(AppliedInvite {
        space_id: from_db(space)?,
        joined_channels: from_db_all(&joined)?,
      });
    }
    tx.commit().await?;
    Ok(applied)
  }
}
