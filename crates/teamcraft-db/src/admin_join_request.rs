//! Requests of workspace admins to join a private channel (admin.md §2.4.1, issue #776).
//!
//! A request is open until an owner approves or declines it, the admin withdraws it, or it expires.
//! A decided request stays until it would have expired: the approving owner's retry is recognised,
//! and an admin can't ask again right after a decline or a withdrawal. Expired rows are deleted by
//! the housekeeping.

use std::collections::BTreeSet;

use teamcraft_proto::{ConversationId, UserId};
use time::OffsetDateTime;

use crate::database::Database;
use crate::error::DbError;
use crate::ids::{from_db, to_db, to_db_all};

/// An open request of `user` to join `conversation`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminJoinRequest {
  pub conversation: ConversationId,
  pub user: UserId,
  pub requested_at: OffsetDateTime,
  pub expires_at: OffsetDateTime,
}

/// What asking found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdminJoinRequestOutcome {
  /// A new request (also after an earlier one of the pair expired or was approved): the owners are
  /// told.
  Created(AdminJoinRequest),
  /// The pair's open request, unchanged: a retry or a second device asked again.
  AlreadyOpen(AdminJoinRequest),
  /// The last request was declined or withdrawn; asking again is possible once it would have
  /// expired.
  TooSoon { until: OffsetDateTime },
}

/// How a request ended without an approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminJoinRequestClosing {
  Declined,
  Withdrawn,
}

/// What an owner's approval did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminJoinApproval {
  /// The request was open; it is approved now and the admin is a member (`is_new_member`: the
  /// membership was added by this approval).
  Approved { is_new_member: bool },
  /// This owner approved it before (a retry after a lost answer); nothing changed.
  ApprovedBefore,
  /// Not open: declined, withdrawn, expired, approved by somebody else, or never asked.
  NotOpen,
}

/// A request that was deleted (expired, or closed by a policy change).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemovedAdminJoinRequest {
  pub conversation: ConversationId,
  pub user: UserId,
  /// Whether it was still waiting for a decision (the owners and the admin are told then).
  pub was_open: bool,
}

impl AdminJoinRequestClosing {
  fn as_str(self) -> &'static str {
    match self {
      Self::Declined => "declined",
      Self::Withdrawn => "withdrawn",
    }
  }
}

impl Database {
  /// Opens a request of `user` for `conversation` that expires `days` after now (the database's
  /// clock), unless one is open already or the last one was declined or withdrawn and hasn't
  /// expired yet. An expired or approved request of the pair is replaced.
  pub async fn open_admin_join_request(
    &self,
    conversation: ConversationId,
    user: UserId,
    days: u32,
  ) -> Result<AdminJoinRequestOutcome, DbError> {
    let mut tx = self.pool().begin().await?;
    sqlx::query!(
      "DELETE FROM admin_join_requests
       WHERE conversation_id = $1 AND user_id = $2 AND (expires_at <= now() OR state = 'approved')",
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
          "SELECT state, requested_at, expires_at FROM admin_join_requests
           WHERE conversation_id = $1 AND user_id = $2",
          to_db(conversation),
          to_db(user),
        )
        .fetch_one(&mut *tx)
        .await?;
        if row.state == "open" {
          AdminJoinRequestOutcome::AlreadyOpen(AdminJoinRequest {
            conversation,
            user,
            requested_at: row.requested_at,
            expires_at: row.expires_at,
          })
        } else {
          AdminJoinRequestOutcome::TooSoon {
            until: row.expires_at,
          }
        }
      },
    };
    tx.commit().await?;
    Ok(outcome)
  }

  /// An owner approves: the open request becomes approved and the admin a member, in one
  /// transaction, so a decline, withdrawal or expiry at the same moment either wins completely or
  /// not at all, and of several owners (or devices) exactly one approves.
  pub async fn approve_admin_join_request(
    &self,
    conversation: ConversationId,
    user: UserId,
    owner: UserId,
  ) -> Result<AdminJoinApproval, DbError> {
    let mut tx = self.pool().begin().await?;
    let is_approved_now = sqlx::query!(
      "UPDATE admin_join_requests SET state = 'approved', decided_by = $3, decided_at = now()
       WHERE conversation_id = $1 AND user_id = $2 AND state = 'open' AND expires_at > now()",
      to_db(conversation),
      to_db(user),
      to_db(owner),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected()
      > 0;
    if !is_approved_now {
      let was_approved_by_owner = sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM admin_join_requests
             WHERE conversation_id = $1 AND user_id = $2 AND state = 'approved' AND decided_by = $3
               AND expires_at > now()) AS "exists!""#,
        to_db(conversation),
        to_db(user),
        to_db(owner),
      )
      .fetch_one(&mut *tx)
      .await?;
      tx.commit().await?;
      return Ok(if was_approved_by_owner {
        AdminJoinApproval::ApprovedBefore
      } else {
        AdminJoinApproval::NotOpen
      });
    }
    let is_new_member = sqlx::query!(
      "INSERT INTO memberships (conversation_id, user_id, role) VALUES ($1, $2, 'member')
       ON CONFLICT DO NOTHING",
      to_db(conversation),
      to_db(user),
    )
    .execute(&mut *tx)
    .await?
    .rows_affected()
      > 0;
    tx.commit().await?;
    Ok(AdminJoinApproval::Approved { is_new_member })
  }

  /// Declines (`by` an owner) or withdraws (`by` the admin) the open request; `false` when none
  /// was open. The row stays until it would have expired.
  pub async fn close_admin_join_request(
    &self,
    conversation: ConversationId,
    user: UserId,
    closing: AdminJoinRequestClosing,
    by: UserId,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE admin_join_requests SET state = $3, decided_by = $4, decided_at = now()
       WHERE conversation_id = $1 AND user_id = $2 AND state = 'open' AND expires_at > now()",
      to_db(conversation),
      to_db(user),
      closing.as_str(),
      to_db(by),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// `user` became a member some other way (`by` added them): their open request is settled as
  /// approved. `false` when none was open.
  pub async fn settle_admin_join_request(
    &self,
    conversation: ConversationId,
    user: UserId,
    by: UserId,
  ) -> Result<bool, DbError> {
    let result = sqlx::query!(
      "UPDATE admin_join_requests SET state = 'approved', decided_by = $3, decided_at = now()
       WHERE conversation_id = $1 AND user_id = $2 AND state = 'open'",
      to_db(conversation),
      to_db(user),
      to_db(by),
    )
    .execute(self.pool())
    .await?;
    Ok(result.rows_affected() > 0)
  }

  /// The open requests `viewer` sees, oldest first: their own, and those for channels they own
  /// (channel role `admin`). Requests that can't be acted on are left out: the channel is archived
  /// or public now; the admin is a member already, left the space, was deactivated or is no admin
  /// any more as far as the server knows.
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
         WHERE r.state = 'open' AND r.expires_at > now()
           AND c.kind = 'channel' AND c.visibility = 'private' AND c.archived_at IS NULL
           AND u.deactivated_at IS NULL AND u.oidc_role IN ('admin', 'owner')
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

  /// The owners (channel role `admin`) of these channels, to tell them about their requests in one
  /// query.
  pub async fn channel_owners(
    &self,
    conversations: &BTreeSet<ConversationId>,
  ) -> Result<Vec<(ConversationId, UserId)>, DbError> {
    let ids: Vec<ConversationId> = conversations.iter().copied().collect();
    let rows = sqlx::query!(
      "SELECT conversation_id, user_id FROM memberships
       WHERE conversation_id = ANY($1) AND role = 'admin'",
      &to_db_all(&ids),
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| Ok((from_db(row.conversation_id)?, from_db(row.user_id)?)))
      .collect()
  }

  /// Deletes up to `limit` requests that ran out, decided or not, and returns them.
  pub async fn take_expired_admin_join_requests(
    &self,
    limit: i64,
  ) -> Result<Vec<RemovedAdminJoinRequest>, DbError> {
    let rows = sqlx::query!(
      r#"DELETE FROM admin_join_requests
         WHERE (conversation_id, user_id) IN (
           SELECT conversation_id, user_id FROM admin_join_requests
           WHERE expires_at <= now() ORDER BY expires_at LIMIT $1)
         RETURNING conversation_id, user_id, state = 'open' AS "was_open!""#,
      limit,
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(RemovedAdminJoinRequest {
          conversation: from_db(row.conversation_id)?,
          user: from_db(row.user_id)?,
          was_open: row.was_open,
        })
      })
      .collect()
  }

  /// Deletes every request and returns them: the policy no longer asks for an owner's approval.
  pub async fn take_all_admin_join_requests(&self) -> Result<Vec<RemovedAdminJoinRequest>, DbError> {
    let rows = sqlx::query!(
      r#"DELETE FROM admin_join_requests
         RETURNING conversation_id, user_id, state = 'open' AND expires_at > now() AS "was_open!""#
    )
    .fetch_all(self.pool())
    .await?;
    rows
      .into_iter()
      .map(|row| {
        Ok(RemovedAdminJoinRequest {
          conversation: from_db(row.conversation_id)?,
          user: from_db(row.user_id)?,
          was_open: row.was_open,
        })
      })
      .collect()
  }
}
