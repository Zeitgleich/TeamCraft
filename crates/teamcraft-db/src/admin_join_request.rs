//! Requests of workspace admins to join a private channel (admin.md §2.4, issue #776). Only open
//! requests are stored: deciding, withdrawing or expiring one deletes it.

use teamcraft_proto::{ConversationId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db};

/// An open request of `user` to join `conversation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminJoinRequest {
  pub conversation: ConversationId,
  pub user: UserId,
  pub requested_at: OffsetDateTime,
  pub expires_at: OffsetDateTime,
}

/// What asking again found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminJoinRequestOutcome {
  /// A new request (also after an earlier one of the pair expired): the owners are told.
  Created(AdminJoinRequest),
  /// The pair's open request, unchanged: a retry or a second device asked again.
  AlreadyOpen(AdminJoinRequest),
}

impl Database {
  /// Opens a request of `user` for `conversation` that expires `days` after now (the database's
  /// clock), unless one is open already. An expired request of the pair is replaced.
  pub async fn open_admin_join_request(
    &self,
    conversation: ConversationId,
    user: UserId,
    days: u32,
  ) -> Result<AdminJoinRequestOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      "DELETE FROM admin_join_requests
       WHERE conversation_id = $1 AND user_id = $2 AND expires_at <= now()",
      to_db(conversation),
      to_db(user),
    )
    .execute(&mut *tx)
    .await?;
    let maybe_created = sqlx::query!(
      r#"INSERT INTO admin_join_requests (conversation_id, user_id, expires_at)
         VALUES ($1, $2, now() + make_interval(days => $3))
         ON CONFLICT (conversation_id, user_id) DO NOTHING
         RETURNING requested_at, expires_at"#,
      to_db(conversation),
      to_db(user),
      i32::try_from(days.max(1)).unwrap_or(i32::MAX),
    )
    .fetch_optional(&mut *tx)
    .await?;
    let outcome = match maybe_created {
      Some(row) => AdminJoinRequestOutcome::Created(AdminJoinRequest {
        conversation,
        user,
        requested_at: row.requested_at,
        expires_at: row.expires_at,
      }),
      None => {
        let row = sqlx::query!(
          "SELECT requested_at, expires_at FROM admin_join_requests
           WHERE conversation_id = $1 AND user_id = $2",
          to_db(conversation),
          to_db(user),
        )
        .fetch_one(&mut *tx)
        .await?;
        AdminJoinRequestOutcome::AlreadyOpen(AdminJoinRequest {
          conversation,
          user,
          requested_at: row.requested_at,
          expires_at: row.expires_at,
        })
      },
    };
    tx.commit().await?;
    Ok(outcome)
  }

  /// The open request of the pair, if there is one.
  pub async fn admin_join_request(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<Option<AdminJoinRequest>, DbError> {
    let maybe_row = sqlx::query!(
      "SELECT requested_at, expires_at FROM admin_join_requests
       WHERE conversation_id = $1 AND user_id = $2 AND expires_at > now()",
      to_db(conversation),
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(maybe_row.map(|row| AdminJoinRequest {
      conversation,
      user,
      requested_at: row.requested_at,
      expires_at: row.expires_at,
    }))
  }

  /// Deletes the open request of the pair and returns it; `None` when there is none (decided,
  /// withdrawn or expired already), so exactly one of several deciders gets it.
  pub async fn take_admin_join_request(
    &self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<Option<AdminJoinRequest>, DbError> {
    let maybe_row = sqlx::query!(
      "DELETE FROM admin_join_requests
       WHERE conversation_id = $1 AND user_id = $2 AND expires_at > now()
       RETURNING requested_at, expires_at",
      to_db(conversation),
      to_db(user),
    )
    .fetch_optional(self.pool())
    .await?;
    Ok(maybe_row.map(|row| AdminJoinRequest {
      conversation,
      user,
      requested_at: row.requested_at,
      expires_at: row.expires_at,
    }))
  }

  /// The open requests `viewer` sees, oldest first: their own, and those for channels they own
  /// (channel role `admin`). Requests that can't be acted on are left out: the channel is archived
  /// or public now, the admin is a member already, left the space or was deactivated.
  pub async fn list_admin_join_requests_for(
    &self,
    viewer: UserId,
    limit: i64,
  ) -> Result<Vec<AdminJoinRequest>, DbError> {
    let rows = sqlx::query!(
      r#"SELECT r.conversation_id, r.user_id, r.requested_at, r.expires_at
         FROM admin_join_requests r
         JOIN conversations c ON c.id = r.conversation_id
         JOIN users u ON u.id = r.user_id
         WHERE r.expires_at > now()
           AND c.kind = 'channel' AND c.visibility = 'private' AND c.archived_at IS NULL
           AND u.deactivated_at IS NULL
           AND EXISTS (SELECT 1 FROM space_members s
                       WHERE s.space_id = c.space_id AND s.user_id = r.user_id
                         AND (s.expires_at IS NULL OR s.expires_at > now()))
           AND NOT EXISTS (SELECT 1 FROM memberships m
                           WHERE m.conversation_id = r.conversation_id AND m.user_id = r.user_id)
           AND (r.user_id = $1
                OR EXISTS (SELECT 1 FROM memberships o
                           WHERE o.conversation_id = r.conversation_id AND o.user_id = $1
                             AND o.role = 'admin'))
         ORDER BY r.requested_at, r.conversation_id, r.user_id
         LIMIT $2"#,
      to_db(viewer),
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(AdminJoinRequest {
          conversation: from_db(row.conversation_id)?,
          user: from_db(row.user_id)?,
          requested_at: row.requested_at,
          expires_at: row.expires_at,
        })
      })
      .collect()
  }

  /// Deletes up to `limit` requests that ran out and returns them, so their owners and admins can
  /// be told.
  pub async fn take_expired_admin_join_requests(
    &self,
    limit: i64,
  ) -> Result<Vec<(ConversationId, UserId)>, DbError> {
    let rows = sqlx::query!(
      "DELETE FROM admin_join_requests
       WHERE (conversation_id, user_id) IN (
         SELECT conversation_id, user_id FROM admin_join_requests
         WHERE expires_at <= now() ORDER BY expires_at LIMIT $1)
       RETURNING conversation_id, user_id",
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.conversation_id)?, from_db(row.user_id)?)))
      .collect()
  }

  /// Deletes every request and returns them: the policy no longer asks for an owner's approval.
  pub async fn take_all_admin_join_requests(&self) -> Result<Vec<(ConversationId, UserId)>, DbError> {
    let rows = sqlx::query!("DELETE FROM admin_join_requests RETURNING conversation_id, user_id")
      .fetch_all(self.pool())
      .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.conversation_id)?, from_db(row.user_id)?)))
      .collect()
  }
}
